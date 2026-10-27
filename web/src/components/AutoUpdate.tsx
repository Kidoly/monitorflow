"use client";

import { useId, useState } from "react";

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
import { administersEverything, useMe } from "@/lib/me";
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
      <NoPermission title="Automatic updates need a global administrator">
        They apply to every customer. Site administrators can override them for a site under Sites.
      </NoPermission>
    );
  }
  return <AutoUpdate />;
}

const PHASES: Record<SitePhase, [string, Tone]> = {
  canary: ["Canary", "info"],
  rolling: ["Rolling out", "info"],
  blocked: ["Canary failed", "bad"],
};

/** One sentence for where the rollout of `kind` stands. */
export function summary(status: AutoUpdateStatus, kind: AutoUpdateKind): [string, Tone] {
  const { settings } = status;
  if (settings.paused) return ["Paused: no automatic update is queued until resumed.", "warn"];
  if (kind.rollout?.state === "halted") return [`Halted: ${kind.rollout.reason ?? "too many updates failed"}.`, "bad"];
  if (!kind.target) {
    return kind.latest
      ? [`The server (${status.server_version}) is older than the newest release, ${kind.latest}: update the server first.`, "warn"]
      : ["The server holds no release yet: import one with monitorflow-server agent-release import.", "neutral"];
  }
  const busy = kind.totals.waiting + kind.totals.pending;
  if (busy === 0) return [`Up to date: every agent where automatic updates apply runs ${kind.target} or newer.`, "ok"];
  return [`Rolling out ${kind.target}: ${kind.totals.pending} in progress, ${kind.totals.waiting} waiting.`, "info"];
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
      setActionError(e instanceof ApiError && e.status === 409 ? `${what}: ${e.message}.` : `${what} failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }
  const change = (body: AutoUpdateChange, what: string) => act(() => api.setAutoUpdate(body), what);

  const header = (
    <PageHeader
      title="Automatic updates"
      meta={
        <>
          Global administrators only
          {(updatedAt !== null || error !== null) && (
            <>
              {" · "}
              <RefreshLine updatedAt={updatedAt} stale={stale} />
            </>
          )}
        </>
      }
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title="Automatic updates need a global administrator" />
      </div>
    );
  }
  if (!data || !updatedAt) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="automatic updates" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const agents = data.agents;
  const [text, tone] = summary(data, agents);
  const halted = agents.rollout?.state === "halted" ? agents.rollout : null;
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
              onClick={() => void act(() => api.resumeRollout(halted.kind, halted.version), `Resuming ${halted.version}`)}
            >
              Resume {halted.version}
            </Button>
          </>
        )}
      </Banner>
      <Settings status={data} busy={busy} onChange={change} />
      <section className={styles.section} aria-label="Agents">
        <div className={styles.sectionHead}>
          <h2 className={styles.sectionTitle}>Agents</h2>
          <span className={styles.dim}>
            Target {agents.target ?? "none"} · newest release held {agents.latest ?? "none"} · server {data.server_version}
          </span>
        </div>
        <StatTiles
          dense
          label="Agents"
          tiles={[
            { label: "Up to date", value: agents.totals.up_to_date, tone: "ok" },
            { label: "Waiting", value: agents.totals.waiting, tone: "neutral" },
            { label: "In progress", value: agents.totals.pending, tone: "info" },
            { label: "Failed", value: agents.totals.failed, tone: agents.totals.failed > 0 ? "bad" : "neutral" },
          ]}
        />
        <Sites sites={agents.sites} />
        <History rollouts={agents.history} now={updatedAt} />
      </section>
    </div>
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
    <section className={styles.section} aria-label="Settings">
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>Settings</h2>
        <span className={styles.dim}>
          Changed <RelativeTime value={status.updated_at} now={new Date()} />
        </span>
      </div>
      <p className={styles.sectionNote}>
        The server moves agents to the newest stable release it holds, never newer than itself and never backwards: one
        agent per site first, then a quarter of the site at a time. A server is tried once per release.
      </p>
      <div className={styles.buttons}>
        <span>
          Server-wide: <strong>{settings.enabled ? "On" : "Off"}</strong>
          {settings.enabled_locked ? (
            <span className={styles.dim}> · set in the server&apos;s configuration (auto_update.enabled)</span>
          ) : (
            <>
              {" "}
              <Button
                size="small"
                disabled={busy}
                onClick={() =>
                  onChange({ enabled: !settings.enabled }, settings.enabled ? "Turning off" : "Turning on")
                }
              >
                {settings.enabled ? "Turn off" : "Turn on"}
              </Button>
            </>
          )}
        </span>
        <Button
          size="small"
          disabled={busy}
          onClick={() => onChange({ paused: !settings.paused }, settings.paused ? "Resuming" : "Pausing")}
        >
          {settings.paused ? "Resume automatic updates" : "Pause automatic updates"}
        </Button>
        <ButtonLink size="small" href={SITES_PATH}>
          Site overrides
        </ButtonLink>
      </div>
      <form
        className={styles.newSite}
        onSubmit={(e) => {
          e.preventDefault();
          if (validThreshold) onChange({ failure_threshold: wanted }, "Changing the threshold");
        }}
      >
        <label className={styles.field} htmlFor={thresholdId}>
          Halt a rollout after this many failed updates
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
          Save
        </Button>
      </form>
    </section>
  );
}

function Sites({ sites }: { sites: AutoUpdateSite[] }) {
  const columns: Column<AutoUpdateSite>[] = [
    {
      key: "site",
      header: "Site",
      cell: (s) => (
        <>
          <strong>{s.name}</strong> <span className={styles.dim}>· {s.customer_name}</span>
        </>
      ),
    },
    {
      key: "policy",
      header: "Automatic",
      cell: (s) => (
        <>
          {s.enabled ? "On" : "Off"}
          {s.auto_update === null && <span className={styles.dim}> · server default</span>}
        </>
      ),
    },
    {
      key: "phase",
      header: "Rollout",
      cell: (s) => {
        if (!s.phase) return <span className={styles.dim}>—</span>;
        const [word, tone] = PHASES[s.phase];
        return <Label tone={tone}>{word}</Label>;
      },
    },
    {
      key: "progress",
      header: "Agents",
      cell: (s) => (
        <span className={styles.dim}>
          {s.totals.up_to_date} up to date · {s.totals.pending} in progress · {s.totals.waiting} waiting
          {s.totals.failed > 0 && ` · ${s.totals.failed} failed`}
        </span>
      ),
    },
  ];
  return <DataTable bare label="Sites" columns={columns} rows={sites} rowKey={(s) => s.id} />;
}

function History({ rollouts, now }: { rollouts: Rollout[]; now: Date }) {
  if (rollouts.length === 0) return null;
  const columns: Column<Rollout>[] = [
    { key: "version", header: "Release", cell: (r) => <strong>{r.version}</strong> },
    {
      key: "state",
      header: "State",
      cell: (r) => (r.state === "halted" ? <Label tone="bad">Halted</Label> : <Label tone="info">Rolling</Label>),
    },
    { key: "started", header: "Started", cell: (r) => <RelativeTime value={r.started_at} now={now} /> },
    { key: "reason", header: "Reason", className: styles.wrap, cell: (r) => r.reason ?? <span className={styles.dim}>—</span> },
  ];
  return <DataTable bare label="Rollouts" columns={columns} rows={rollouts} rowKey={(r) => `${r.kind}-${r.version}`} />;
}
