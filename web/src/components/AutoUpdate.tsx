"use client";

import { useId, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type AutoUpdateChange,
  type AutoUpdateKind,
  type AutoUpdateSite,
  type AutoUpdateStatus,
  type Rollout,
  type SitePhase,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, useMe, useMultiCustomer } from "@/lib/me";
import { SITES_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { Button, ButtonLink, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine, StaleBanner } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { StatTiles } from "./ui/StatTiles";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

/** `/admin/updates/`: global administrators only, like the API (ADR-0055). */
export function AutoUpdatePage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("autoUpdate.role.title")}>{t("autoUpdate.role.text")}</NoPermission>
    );
  }
  return <AutoUpdate />;
}

const PHASE_TONES: Record<SitePhase, Tone> = {
  canary: "info",
  rolling: "info",
  blocked: "bad",
};

/** One sentence for where the rollout of `kind` stands. */
export function summary(status: AutoUpdateStatus, kind: AutoUpdateKind): [string, Tone] {
  const { settings } = status;
  if (settings.paused) return [t("autoUpdate.summary.paused"), "warn"];
  if (kind.rollout?.state === "halted") {
    // The server's reason stays in its words (ADR-0136).
    return [t("autoUpdate.summary.halted", { reason: kind.rollout.reason ?? t("autoUpdate.summary.tooManyFailed") }), "bad"];
  }
  if (!kind.target) {
    return kind.latest
      ? [t("autoUpdate.summary.serverOlder", { server: status.server_version, latest: kind.latest }), "warn"]
      : [t("autoUpdate.summary.noRelease"), "neutral"];
  }
  const busy = kind.totals.waiting + kind.totals.pending;
  if (busy === 0) return [t("autoUpdate.summary.upToDate", { target: kind.target }), "ok"];
  return [
    t("autoUpdate.summary.rolling", { target: kind.target, pending: kind.totals.pending, waiting: kind.totals.waiting }),
    "info",
  ];
}

function AutoUpdate() {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(api.autoUpdate);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  async function act(action: () => Promise<unknown>, what: string) {
    setBusy(true);
    setActionError(null);
    try {
      await action();
      refresh();
    } catch (e) {
      setActionError(
        e instanceof ApiError && e.status === 409
          ? t("autoUpdate.refused", { what, reason: e.message })
          : t("autoUpdate.failed", { what, error: describeError(e) }),
      );
    } finally {
      setBusy(false);
    }
  }
  const change = (body: AutoUpdateChange, what: string) => act(() => api.setAutoUpdate(body), what);

  const header = (
    <PageHeader
      title={t("autoUpdate.title")}
      meta={updatedAt !== null || error !== null ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : undefined}
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("autoUpdate.role.title")} />
      </div>
    );
  }
  if (!data || !updatedAt) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("autoUpdate.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const agents = data.agents;
  const [text, tone] = summary(data, agents);
  const halted = [data.relays.rollout, agents.rollout].find((r) => r?.state === "halted") ?? null;
  return (
    <div className={styles.page}>
      {header}
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <Banner tone={tone === "neutral" ? "plain" : tone} role={tone === "bad" ? "alert" : "status"}>
        {text}
        {halted && (
          <>
            {" "}
            <Button
              size="small"
              disabled={busy}
              onClick={() =>
                void act(
                  () => api.resumeRollout(halted.kind, halted.version),
                  t("autoUpdate.resuming", { version: halted.version }),
                )
              }
            >
              {t("autoUpdate.resume", { version: halted.version })}
            </Button>
          </>
        )}
      </Banner>
      <Settings status={data} busy={busy} onChange={change} />
      <Kind relays kind={data.relays} status={data} now={updatedAt} />
      <Kind relays={false} kind={agents} status={data} now={updatedAt} />
    </div>
  );
}

/** Progress of one kind of peer: totals, sites and rollouts. */
function Kind({
  relays,
  kind,
  status,
  now,
}: {
  relays: boolean;
  kind: AutoUpdateKind;
  status: AutoUpdateStatus;
  now: Date;
}) {
  const title = relays ? t("autoUpdate.relays") : t("autoUpdate.agents");
  return (
    <section className={styles.section} aria-label={title}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{title}</h2>
        <span className={styles.dim}>
          {t("autoUpdate.facts", {
            target: kind.target ?? t("autoUpdate.none"),
            latest: kind.latest ?? t("autoUpdate.none"),
            server: status.server_version,
          })}
        </span>
      </div>
      {relays ? (
        <p className={styles.sectionNote}>{t("autoUpdate.relaysFirst")}</p>
      ) : (
        kind.totals.waiting_for_relay > 0 && (
          <p className={styles.sectionNote}>
            {t("autoUpdate.waitingForRelay", { count: kind.totals.waiting_for_relay })}
          </p>
        )
      )}
      <StatTiles
        dense
        label={title}
        tiles={[
          { label: t("autoUpdate.tile.upToDate"), value: kind.totals.up_to_date, tone: "ok" },
          { label: t("autoUpdate.tile.waiting"), value: kind.totals.waiting, tone: "neutral" },
          { label: t("autoUpdate.tile.inProgress"), value: kind.totals.pending, tone: "info" },
          {
            label: t("autoUpdate.tile.failed"),
            value: kind.totals.failed,
            tone: kind.totals.failed > 0 ? "bad" : "neutral",
          },
        ]}
      />
      <Sites
        sites={kind.sites}
        label={relays ? t("autoUpdate.relaysBySite") : t("autoUpdate.agentsBySite")}
        relays={relays}
      />
      <History
        rollouts={kind.history}
        now={now}
        label={relays ? t("autoUpdate.relaysRollouts") : t("autoUpdate.agentsRollouts")}
      />
    </section>
  );
}

function Settings({
  status,
  busy,
  onChange,
}: {
  status: AutoUpdateStatus;
  busy: boolean;
  onChange: (change: AutoUpdateChange, what: string) => void;
}) {
  const { settings } = status;
  const thresholdId = useId();
  const [threshold, setThreshold] = useState(String(settings.failure_threshold));
  const wanted = Number(threshold);
  const validThreshold = Number.isInteger(wanted) && wanted >= 1 && wanted <= 1000;
  return (
    <section className={styles.section} aria-label={t("autoUpdate.settings")}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("autoUpdate.settings")}</h2>
      </div>
      <p className={styles.sectionNote}>{t("autoUpdate.explain")}</p>
      <div className={styles.buttons}>
        <span>
          {t("autoUpdate.serverWide")} <strong>{settings.enabled ? t("autoUpdate.on") : t("autoUpdate.off")}</strong>
          {settings.enabled_locked ? (
            <span className={styles.dim}>{t("autoUpdate.locked")}</span>
          ) : (
            <>
              {" "}
              <Button
                size="small"
                disabled={busy}
                onClick={() =>
                  onChange(
                    { enabled: !settings.enabled },
                    settings.enabled ? t("autoUpdate.turningOff") : t("autoUpdate.turningOn"),
                  )
                }
              >
                {settings.enabled ? t("autoUpdate.turnOff") : t("autoUpdate.turnOn")}
              </Button>
            </>
          )}
        </span>
        <Button
          size="small"
          disabled={busy}
          onClick={() =>
            onChange({ paused: !settings.paused }, settings.paused ? t("autoUpdate.resumingAll") : t("autoUpdate.pausing"))
          }
        >
          {settings.paused ? t("autoUpdate.resumeAll") : t("autoUpdate.pauseAll")}
        </Button>
        <ButtonLink size="small" href={SITES_PATH}>
          {t("autoUpdate.overrides")}
        </ButtonLink>
      </div>
      <form
        className={styles.newSite}
        onSubmit={(e) => {
          e.preventDefault();
          if (validThreshold) onChange({ failure_threshold: wanted }, t("autoUpdate.changingThreshold"));
        }}
      >
        <label className={styles.field} htmlFor={thresholdId}>
          {t("autoUpdate.threshold")}
        </label>
        <input
          id={thresholdId}
          className={fieldClass}
          type="number"
          min={1}
          max={1000}
          value={threshold}
          onChange={(e) => setThreshold(e.target.value)}
        />
        <Button type="submit" size="small" disabled={busy || !validThreshold || wanted === settings.failure_threshold}>
          {t("autoUpdate.save")}
        </Button>
      </form>
    </section>
  );
}

function Sites({ sites, label, relays }: { sites: AutoUpdateSite[]; label: string; relays: boolean }) {
  const multiCustomer = useMultiCustomer();
  const columns: Column<AutoUpdateSite>[] = [
    {
      key: "site",
      header: t("autoUpdate.column.site"),
      cell: (s) => (
        <>
          <strong>{s.name}</strong> {multiCustomer && <span className={styles.dim}>· {s.customer_name}</span>}
        </>
      ),
    },
    {
      key: "policy",
      header: t("autoUpdate.column.automatic"),
      cell: (s) => (
        <>
          {s.enabled ? t("autoUpdate.on") : t("autoUpdate.off")}
          {s.auto_update === null && <span className={styles.dim}>{t("autoUpdate.serverDefault")}</span>}
        </>
      ),
    },
    {
      key: "phase",
      header: t("autoUpdate.column.rollout"),
      cell: (s) => {
        if (!s.phase) return null;
        return <Label tone={PHASE_TONES[s.phase]}>{t(`autoUpdate.phase.${s.phase}`)}</Label>;
      },
    },
    {
      key: "progress",
      header: relays ? t("autoUpdate.relays") : t("autoUpdate.agents"),
      cell: (s) => (
        <span className={styles.dim}>
          {t("autoUpdate.progress", { upToDate: s.totals.up_to_date, pending: s.totals.pending, waiting: s.totals.waiting })}
          {s.totals.failed > 0 && t("autoUpdate.progressFailed", { failed: s.totals.failed })}
        </span>
      ),
    },
  ];
  return <DataTable bare label={label} columns={columns} rows={sites} rowKey={(s) => s.id} />;
}

function History({ rollouts, now, label }: { rollouts: Rollout[]; now: Date; label: string }) {
  if (rollouts.length === 0) return null;
  const columns: Column<Rollout>[] = [
    { key: "version", header: t("autoUpdate.column.release"), cell: (r) => <strong>{r.version}</strong> },
    {
      key: "state",
      header: t("autoUpdate.column.state"),
      cell: (r) =>
        r.state === "halted" ? (
          <Label tone="bad">{t("autoUpdate.halted")}</Label>
        ) : (
          <Label tone="info">{t("autoUpdate.rolling")}</Label>
        ),
    },
    { key: "started", header: t("autoUpdate.column.started"), cell: (r) => <RelativeTime value={r.started_at} now={now} /> },
    {
      key: "reason",
      header: t("autoUpdate.column.reason"),
      className: styles.wrap,
      cell: (r) => r.reason,
    },
  ];
  return <DataTable bare label={label} columns={columns} rows={rollouts} rowKey={(r) => `${r.kind}-${r.version}`} />;
}
