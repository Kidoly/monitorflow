"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

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

/** The word for where a server's packages come from. */
function standing(status: SourcesStatus): [string, Tone] {
  if (status.changing_to === "mirror") return ["Moving to a mirror", "info"];
  if (status.changing_to === "own") return ["Going back to its own sources", "info"];
  return status.using === "mirror" ? ["Uses a mirror", "ok"] : ["Uses its own sources", "neutral"];
}

/** "debian-12, debian-12-updates at snapshot 2026…": snapshots only when pinned. */
function mirrorsText(status: SourcesStatus): string {
  return status.mirrors
    .map((m) => (m.snapshot === "current" ? m.name : `${m.name} at snapshot ${m.snapshot}`))
    .join(", ");
}

const USE_CONSEQUENCE =
  "Its other package sources are set aside and its packages come from the mirror, through its relay when the relay has a copy. " +
  "If the mirror doesn't answer, the change undoes itself. You can go back at any time.";
const USE_SITE_CONSEQUENCE =
  "Their other package sources are set aside and their packages come from the mirror, through the relay when it has a copy. " +
  "On a server the mirror doesn't answer, the change undoes itself. You can go back at any time.";
const BACK_CONSEQUENCE = "Its original package sources are put back exactly as they were.";
const FAILED = "A change that fails puts the server's own sources back.";

/** The section of a server's Updates tab (ADR-0074). */
export function PackageSources({ server }: { server: ServerDetail }) {
  const me = useMe();
  const id = server.id;
  const fetcher = useCallback((signal: AbortSignal) => api.serverSources(id, signal), [id]);
  const { data: status, error, refresh } = usePolledData(fetcher);
  const canChange = administersCustomer(me, server.customer_id);

  if (!status) {
    return error ? (
      <ErrorState what="the package sources" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading the package sources" />
    );
  }
  const [word, tone] = standing(status);
  const settled = status.changing_to === null && !status.campaign;
  return (
    <section className={admin.section} aria-labelledby="sources-title">
      <div className={styles.line}>
        <h2 id="sources-title" className={admin.sectionTitle}>
          Package sources
        </h2>
        <Label tone={tone}>{word}</Label>
      </div>
      {status.failed && (
        <Banner tone="bad">
          The last change didn&apos;t work: {status.failed} {FAILED}
        </Banner>
      )}
      <p className={admin.sectionNote}>
        {status.using === "mirror"
          ? `${server.hostname} gets its packages from ${mirrorsText(status)}.`
          : `${server.hostname} gets its packages from the sources it came with.`}{" "}
        {status.changing_to && "The change reaches it at its next contact."}
        {status.campaign && "An update campaign has its sources for now and puts them back when it is done."}
      </p>
      {status.using === "own" && status.problem && <Problem status={status} />}
      {canChange && settled && status.using === "own" && !status.problem && (
        <div className={styles.actions}>
          <InlineConfirm
            trigger={`Use the ${status.distribution} mirror…`}
            triggerVariant="k"
            title={`Use the mirror on ${server.hostname}?`}
            consequence={USE_CONSEQUENCE}
            confirmLabel={`Use the mirror on ${server.hostname}`}
            pendingLabel="Sending…"
            onConfirm={async () => {
              await api.useMirror(id);
              refresh();
            }}
            describeError={(e) => `Nothing changed. ${describeError(e)}`}
          />
        </div>
      )}
      {canChange && settled && status.using === "mirror" && (
        <div className={styles.actions}>
          <InlineConfirm
            trigger="Go back to its own sources…"
            triggerVariant="default"
            title={`Put back the own sources of ${server.hostname}?`}
            consequence={BACK_CONSEQUENCE}
            confirmLabel={`Go back on ${server.hostname}`}
            pendingLabel="Sending…"
            onConfirm={async () => {
              await api.revertSources(id);
              refresh();
            }}
            describeError={(e) => `Nothing changed. ${describeError(e)}`}
          />
        </div>
      )}
    </section>
  );
}

/** Why the server keeps its sources, and the next step when there is one. */
function Problem({ status }: { status: SourcesStatus }) {
  const me = useMe();
  return (
    <>
      <Banner tone="plain">{status.problem}</Banner>
      {status.add_mirror &&
        (administersEverything(me) ? (
          <div className={styles.actions}>
            <ButtonLink href={MIRRORS_PATH}>Add {status.distribution} on the Mirrors page</ButtonLink>
          </div>
        ) : (
          <p className={admin.sectionNote}>A global administrator can add it on the Mirrors page.</p>
        ))}
    </>
  );
}

const count = (n: number) => `${n} ${n === 1 ? "server" : "servers"}`;

/** What a change of the whole site did, server by server. */
function ChangeResult({ change }: { change: SiteSourcesChange }) {
  return (
    <Banner tone={change.queued.length > 0 ? "ok" : "warn"} role="status">
      {change.queued.length > 0
        ? `Sent to ${count(change.queued.length)}. Each one changes at its next contact.`
        : "No server was changed."}
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

type SiteRow = SourcesStatus & { id: string; hostname: string };

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
      <ErrorState what="the package sources" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading the package sources" />
    );
  }
  if (data.servers.length === 0) return null;
  const free = (s: SiteRow) => s.changing_to === null && !s.campaign;
  const canUse = data.servers.filter((s) => s.using === "own" && !s.problem && free(s)).length;
  const canGoBack = data.servers.filter((s) => s.using === "mirror" && free(s)).length;
  const columns: Column<SiteRow>[] = [
    {
      key: "server",
      header: "Server",
      cell: (s) => <Link href={serverHref(s.id, "updates")}>{s.hostname}</Link>,
    },
    {
      key: "sources",
      header: "Package sources",
      cell: (s) => {
        const [word, tone] = standing(s);
        return <Label tone={tone}>{word}</Label>;
      },
    },
    {
      key: "note",
      header: "Details",
      className: styles.reason,
      cell: (s) => (
        <span className={styles.dim}>
          {s.failed
            ? `The last change didn't work: ${s.failed}`
            : s.campaign
              ? "An update campaign has its sources for now."
              : s.using === "mirror"
                ? mirrorsText(s)
                : (s.problem ?? `Can use ${s.distribution}.`)}
        </span>
      ),
    },
  ];
  return (
    <section className={admin.section} aria-labelledby="site-sources-title">
      <h2 id="site-sources-title" className={admin.sectionTitle}>
        Package sources
      </h2>
      <p className={admin.sectionNote}>
        Point the servers of {site.name} at the mirror of their own distribution, or back at the sources they came with.
        A server whose distribution has no mirror is left as it is.
      </p>
      {change && <ChangeResult change={change} />}
      <DataTable dense label="Package sources of the site's servers" columns={columns} rows={data.servers} rowKey={(s) => s.id} />
      {canChange && (canUse > 0 || canGoBack > 0) && (
        <div className={styles.actions}>
          {canUse > 0 && (
            <InlineConfirm
              trigger={`Use mirrors on ${count(canUse)}…`}
              triggerVariant="k"
              title={`Use mirrors on ${count(canUse)} of ${site.name}?`}
              consequence={USE_SITE_CONSEQUENCE}
              confirmLabel={`Use mirrors on ${count(canUse)}`}
              pendingLabel="Sending…"
              onConfirm={async () => {
                setChange(await api.useMirrorOnSite(id));
                refresh();
              }}
              describeError={(e) => `Nothing changed. ${describeError(e)}`}
            />
          )}
          {canGoBack > 0 && (
            <InlineConfirm
              trigger={`Go back on ${count(canGoBack)}…`}
              triggerVariant="default"
              title={`Put back the own sources of ${count(canGoBack)}?`}
              consequence="Their original package sources are put back exactly as they were."
              confirmLabel={`Go back on ${count(canGoBack)}`}
              pendingLabel="Sending…"
              onConfirm={async () => {
                setChange(await api.revertSiteSources(id));
                refresh();
              }}
              describeError={(e) => `Nothing changed. ${describeError(e)}`}
            />
          )}
        </div>
      )}
    </section>
  );
}
