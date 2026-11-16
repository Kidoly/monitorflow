"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { t } from "@/i18n";
import { type ServerDetail, type Site, type SiteSourcesChange, type SourcesStatus, api, describeError } from "@/lib/api";
import { administersCustomer, administersEverything, useMe } from "@/lib/me";
import { MIRRORS_PATH, serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./PackageSources.module.css";
import admin from "./AdminPages.module.css";
import { ButtonLink } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";
import { UpdateSource, describeUpdateSource } from "./UpdateSource";

/** The word for where a server's packages come from. */
function standing(status: SourcesStatus): [string, Tone] {
  if (status.changing_to === "mirror") return [t("mirrors.sources.movingToMirror"), "info"];
  if (status.changing_to === "own") return [t("mirrors.sources.goingBack"), "info"];
  return status.using === "mirror"
    ? [t("mirrors.sources.usesMirror"), "ok"]
    : [t("mirrors.sources.usesOwn"), "neutral"];
}

/** "debian-12, debian-12-updates at snapshot 2026…": snapshots only when pinned. */
function mirrorsText(status: SourcesStatus): string {
  return status.mirrors
    .map((m) => (m.snapshot === "current" ? m.name : t("mirrors.sources.atSnapshot", { name: m.name, snapshot: m.snapshot })))
    .join(", ");
}


/** The section of a server's Updates tab (ADR-0074); for Windows, its update source (ADR-0086). */
export function PackageSources({ server }: { server: ServerDetail }) {
  const me = useMe();
  const id = server.id;
  const fetcher = useCallback((signal: AbortSignal) => api.serverSources(id, signal), [id]);
  const { data: status, error, refresh } = usePolledData(fetcher);
  const canChange = administersCustomer(me, server.customer_id);

  if (!status) {
    return error ? (
      <ErrorState what={t("mirrors.sources.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("mirrors.sources.loading")} />
    );
  }
  if (server.os_family === "windows") return <UpdateSource source={status.windows_update_source} />;
  const [word, tone] = standing(status);
  const settled = status.changing_to === null && !status.campaign;
  return (
    <section className={admin.section} aria-labelledby="sources-title">
      <div className={admin.sectionHead}>
        <h2 id="sources-title" className={admin.sectionTitle}>
          {t("mirrors.sources.title")}
        </h2>
        <Label tone={tone}>{word}</Label>
      </div>
      <div className={styles.body}>
        {status.failed && (
          <Banner tone="bad">{t("mirrors.sources.failed", { reason: status.failed })}</Banner>
        )}
        {/* Its own sources go without saying: the label above says it. */}
        {(status.using === "mirror" || status.changing_to || status.campaign) && (
          <p className={styles.note}>
            {[
              status.using === "mirror" && t("mirrors.sources.fromMirror", { server: server.hostname, mirrors: mirrorsText(status) }),
              status.changing_to && t("mirrors.sources.nextContact"),
              status.campaign && t("mirrors.sources.campaign"),
            ]
              .filter(Boolean)
              .join(" ")}
          </p>
        )}
        {status.using === "own" && status.problem && <Problem status={status} osVersion={server.os_version} />}
        {canChange && settled && status.using === "own" && !status.problem && (
          <div className={styles.actions}>
            <InlineConfirm
              trigger={t("mirrors.sources.use.trigger", { distribution: status.distribution ?? "" })}
              triggerVariant="k"
              title={t("mirrors.sources.use.title", { server: server.hostname })}
              consequence={t("mirrors.sources.useConsequence")}
              confirmLabel={t("mirrors.sources.use.confirm", { server: server.hostname })}
              pendingLabel={t("mirrors.sources.sending")}
              onConfirm={async () => {
                await api.useMirror(id);
                refresh();
              }}
              describeError={(e) => t("mirrors.sources.nothingChanged", { error: describeError(e) })}
            />
          </div>
        )}
        {canChange && settled && status.using === "mirror" && (
          <div className={styles.actions}>
            <InlineConfirm
              trigger={t("mirrors.sources.back.trigger")}
              triggerVariant="default"
              title={t("mirrors.sources.back.title", { server: server.hostname })}
              consequence={t("mirrors.sources.backConsequence")}
              confirmLabel={t("mirrors.sources.back.confirm", { server: server.hostname })}
              pendingLabel={t("mirrors.sources.sending")}
              onConfirm={async () => {
                await api.revertSources(id);
                refresh();
              }}
              describeError={(e) => t("mirrors.sources.nothingChanged", { error: describeError(e) })}
            />
          </div>
        )}
      </div>
    </section>
  );
}

/**
 * Why `status` cannot use a mirror, in the active language from its code;
 * the API's English sentence from an older server.
 */
export function problemText(status: Pick<SourcesStatus, "problem" | "problem_code" | "distribution">, osVersion: string | null): string | null {
  const code = status.problem_code;
  if (!code) return status.problem;
  return t(`mirrors.sources.problem.${code}`, { distribution: status.distribution ?? "", version: osVersion ?? "" });
}

/**
 * Why the server keeps its sources, and the next step when there is one.
 * When the step is adding its distribution, the label and the button say it all.
 */
function Problem({ status, osVersion }: { status: SourcesStatus; osVersion: string | null }) {
  const me = useMe();
  const add = status.add_mirror && administersEverything(me);
  return (
    <>
      {!add && <Banner tone="plain">{problemText(status, osVersion)}</Banner>}
      {status.add_mirror &&
        (administersEverything(me) ? (
          <div className={styles.actions}>
            <ButtonLink href={MIRRORS_PATH}>
              {t("mirrors.sources.addOnMirrors", { distribution: status.distribution ?? "" })}
            </ButtonLink>
          </div>
        ) : (
          <p className={styles.note}>{t("mirrors.sources.askGlobal")}</p>
        ))}
    </>
  );
}

/** A Windows row of the site table: where its updates come from, and what is wrong. */
function windowsDetails(s: SiteRow): string {
  const words = describeUpdateSource(s.windows_update_source);
  return [words.text, ...words.warnings].join(" ");
}

const count = (n: number) => t("mirrors.sources.servers", { count: n });

/** What a change of the whole site did, server by server. */
function ChangeResult({ change }: { change: SiteSourcesChange }) {
  return (
    <Banner tone={change.queued.length > 0 ? "ok" : "warn"} role="status">
      {change.queued.length > 0
        ? t("mirrors.sources.sent", { servers: count(change.queued.length) })
        : t("mirrors.sources.noneChanged")}
      {change.skipped.length > 0 && (
        <ul className={styles.skipped}>
          {change.skipped.map((s) => (
            <li key={s.id}>
              {s.hostname}: {s.reason}
            </li>
          ))}
        </ul>
      )}
    </Banner>
  );
}

type SiteRow = SourcesStatus & { id: string; hostname: string; os_family: string };

/** Every server of a site at once, on the site page (ADR-0074). */
export function SiteSources({ site }: { site: Site }) {
  const me = useMe();
  const id = site.id;
  const fetcher = useCallback((signal: AbortSignal) => api.siteSources(id, signal), [id]);
  const { data, error, refresh } = usePolledData(fetcher);
  const [change, setChange] = useState<SiteSourcesChange | null>(null);
  const canChange = administersCustomer(me, site.customer_id);

  if (!data) {
    return error ? (
      <ErrorState what={t("mirrors.sources.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("mirrors.sources.loading")} />
    );
  }
  if (data.servers.length === 0) return null;
  const free = (s: SiteRow) => s.changing_to === null && !s.campaign;
  const canUse = data.servers.filter((s) => s.using === "own" && !s.problem && free(s)).length;
  const canGoBack = data.servers.filter((s) => s.using === "mirror" && free(s)).length;
  const columns: Column<SiteRow>[] = [
    {
      key: "server",
      header: t("mirrors.sources.column.server"),
      cell: (s) => <Link href={serverHref(s.id, "updates")}>{s.hostname}</Link>,
    },
    {
      key: "sources",
      header: t("mirrors.sources.title"),
      cell: (s) => {
        if (s.os_family === "windows") {
          const words = describeUpdateSource(s.windows_update_source);
          return <Label tone={words.tone}>{words.label}</Label>;
        }
        const [word, tone] = standing(s);
        return <Label tone={tone}>{word}</Label>;
      },
    },
    {
      key: "note",
      header: t("mirrors.sources.column.details"),
      className: styles.reason,
      cell: (s) => (
        <span className={styles.dim}>
          {s.os_family === "windows"
            ? windowsDetails(s)
            : s.failed
              ? t("mirrors.sources.lastFailed", { reason: s.failed })
              : s.campaign
                ? t("mirrors.sources.campaignShort")
                : s.using === "mirror"
                  ? mirrorsText(s)
                  : (s.problem ?? t("mirrors.sources.canUse", { distribution: s.distribution ?? "" }))}
        </span>
      ),
    },
  ];
  return (
    <section className={admin.section} aria-labelledby="site-sources-title">
      <div className={admin.sectionHead}>
        <h2 id="site-sources-title" className={admin.sectionTitle}>
          {t("mirrors.sources.title")}
        </h2>
      </div>
      <div className={styles.body}>
        <p className={styles.note}>{t("mirrors.sources.site", { site: site.name })}</p>
        {change && <ChangeResult change={change} />}
        <DataTable
          dense
          label={t("mirrors.sources.siteTable")}
          columns={columns}
          rows={data.servers}
          rowKey={(s) => s.id}
        />
        {canChange && (canUse > 0 || canGoBack > 0) && (
          <div className={styles.actions}>
            {canUse > 0 && (
              <InlineConfirm
                trigger={t("mirrors.sources.useSite.trigger", { servers: count(canUse) })}
                triggerVariant="k"
                title={t("mirrors.sources.useSite.title", { servers: count(canUse), site: site.name })}
                consequence={t("mirrors.sources.useSiteConsequence")}
                confirmLabel={t("mirrors.sources.useSite.confirm", { servers: count(canUse) })}
                pendingLabel={t("mirrors.sources.sending")}
                onConfirm={async () => {
                  setChange(await api.useMirrorOnSite(id));
                  refresh();
                }}
                describeError={(e) => t("mirrors.sources.nothingChanged", { error: describeError(e) })}
              />
            )}
            {canGoBack > 0 && (
              <InlineConfirm
                trigger={t("mirrors.sources.backSite.trigger", { servers: count(canGoBack) })}
                triggerVariant="default"
                title={t("mirrors.sources.backSite.title", { servers: count(canGoBack) })}
                consequence={t("mirrors.sources.backSiteConsequence")}
                confirmLabel={t("mirrors.sources.backSite.confirm", { servers: count(canGoBack) })}
                pendingLabel={t("mirrors.sources.sending")}
                onConfirm={async () => {
                  setChange(await api.revertSiteSources(id));
                  refresh();
                }}
                describeError={(e) => t("mirrors.sources.nothingChanged", { error: describeError(e) })}
              />
            )}
          </div>
        )}
      </div>
    </section>
  );
}
