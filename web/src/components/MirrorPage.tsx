"use client";

import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useState } from "react";

import { ApiError, type MirrorDetail, type MirrorRun, type MirrorSnapshot, api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { administersEverything, useMe } from "@/lib/me";
import { describeMirror, durationText, intervalText, minuteText } from "@/lib/mirrors";
import { MIRRORS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import { nextSyncText, useMirrorJobs } from "./MirrorList";
import styles from "./Mirrors.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, ButtonLink } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, NotFound, PageHeader, StaleBanner } from "./ui/PageState";
import { StatTiles } from "./ui/StatTiles";
import { Label, StatusLabel, statusWord } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

const SNAPSHOT_STATES: Record<MirrorSnapshot["state"], [string, Tone]> = {
  creating: ["Creating", "info"],
  published: ["Published", "ok"],
  failed: ["Failed", "bad"],
};

const LAST_RUN: Record<MirrorRun["state"] | "never", [string, Tone]> = {
  running: ["Running", "info"],
  succeeded: ["Last run succeeded", "ok"],
  failed: ["Last run failed", "bad"],
  never: ["Never run", "neutral"],
};

/** `/admin/mirrors/view/?id=…` (ADR-0020 routing). */
export function MirrorView() {
  const id = useSearchParams().get("id");
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Mirrors need a global administrator">
        Mirrors are shared by every customer. Ask a global administrator for a role.
      </NoPermission>
    );
  }
  return <MirrorPage key={id} id={id} />;
}

const notFound = (
  <NotFound
    title="Mirror not found"
    actions={<ButtonLink href={MIRRORS_PATH}>Back to mirrors</ButtonLink>}
  >
    It may have been removed.
  </NotFound>
);

export function MirrorPage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const fetcher = useCallback((signal: AbortSignal) => api.mirror(id ?? "", signal), [id]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });
  const jobs = useMirrorJobs(refresh);
  const [open, setOpen] = useState<Set<string>>(() => new Set());

  useEffect(() => {
    if (data) document.title = `${data.name} · MonitorFlow`;
  }, [data]);

  if (!valid || (error instanceof ApiError && error.status === 404)) return notFound;
  if (!data || !updatedAt) {
    return error ? <ErrorState what="the mirror" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />;
  }

  const mirror = data;
  const running = mirror.last_run?.state === "running";
  const [word, tone] = LAST_RUN[mirror.last_run?.state ?? "never"];
  const lastSync = mirror.runs.find((r) => r.kind === "sync") ?? (mirror.runs.length === 0 ? null : undefined);
  const disabled = jobs.busy !== null || running;
  const toggle = (runId: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (!next.delete(runId)) next.add(runId);
      return next;
    });

  return (
    <div className={styles.page}>
      <Breadcrumb parents={[{ label: "Mirrors", href: MIRRORS_PATH }]} current={mirror.name} />
      <PageHeader
        detail
        title={mirror.name}
        status={<Label tone={tone}>{word}</Label>}
        meta={`${describeMirror(mirror, { distribution: true })} · ${intervalText(mirror.sync_interval_s)}`}
        actions={
          <>
            <Button disabled={disabled} onClick={() => void jobs.start(mirror, "snapshot")}>
              Create snapshot
            </Button>
            <Button variant="k" disabled={disabled} onClick={() => void jobs.start(mirror, "sync")}>
              Sync now
            </Button>
          </>
        }
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {jobs.error && <Banner tone="bad">{jobs.error}</Banner>}
      <StatTiles
        dense
        label="Summary"
        tiles={[
          { label: "Snapshots", value: mirror.snapshots.filter((s) => s.state === "published").length, tone: "neutral" },
          {
            label: "Last run",
            value: mirror.last_run ? statusWord("run", mirror.last_run.state)[0] : "Never",
            tone,
          },
          { label: "Next sync", value: nextSyncText(mirror, lastSync, updatedAt), tone: "neutral" },
        ]}
      />
      <Snapshots mirror={mirror} />
      <Runs mirror={mirror} open={open} onToggle={toggle} />
    </div>
  );
}

function Snapshots({ mirror }: { mirror: MirrorDetail }) {
  if (mirror.snapshots.length === 0) {
    return (
      <EmptyState title="No snapshots yet">
        {mirror.auto_snapshot ? "A snapshot is published after each successful sync." : "Create one after a sync."}
      </EmptyState>
    );
  }
  const columns: Column<MirrorSnapshot>[] = [
    { key: "name", header: "Snapshot", className: styles.strong, cell: (s) => s.name },
    {
      key: "state",
      header: "State",
      cell: (s) => (
        <>
          <Label tone={SNAPSHOT_STATES[s.state][1]}>{SNAPSHOT_STATES[s.state][0]}</Label>
          {s.state === "published" && s.name === mirror.current_snapshot && <span className={styles.dim}> current</span>}
          {s.error && <span className={styles.dim}> {s.error}</span>}
        </>
      ),
    },
    { key: "size", header: "Size", cell: (s) => (s.size_bytes === null ? <Dash /> : formatBytes(s.size_bytes)) },
    { key: "created", header: "Created", cell: (s) => minuteText(s.created_at) },
    { key: "published", header: "Published", cell: (s) => (s.published_at ? minuteText(s.published_at) : <Dash />) },
  ];
  return <DataTable dense label="Snapshots" columns={columns} rows={mirror.snapshots} rowKey={(s) => s.id} />;
}

function Runs({ mirror, open, onToggle }: { mirror: MirrorDetail; open: Set<string>; onToggle: (id: string) => void }) {
  if (mirror.runs.length === 0) {
    return <EmptyState>No runs yet.</EmptyState>;
  }
  const snapshotName = (id: string | null) => mirror.snapshots.find((s) => s.id === id)?.name;
  const summary = (run: MirrorRun) => {
    if (run.error) return run.error;
    if (run.state === "running") return run.kind === "sync" ? "Syncing" : "Creating a snapshot";
    const name = snapshotName(run.snapshot_id);
    if (run.kind === "snapshot") return name ? `Snapshot ${name} published` : "Snapshot published";
    return name ? `Synced, snapshot ${name} published` : "Synced";
  };
  const columns: Column<MirrorRun>[] = [
    { key: "run", header: "Run", className: styles.strong, cell: (r) => minuteText(r.started_at) },
    { key: "state", header: "State", cell: (r) => <StatusLabel kind="run" value={r.state} /> },
    {
      key: "duration",
      header: "Duration",
      cell: (r) =>
        r.finished_at ? (
          durationText(new Date(r.finished_at).getTime() - new Date(r.started_at).getTime())
        ) : (
          <Dash />
        ),
    },
    { key: "trigger", header: "Trigger", cell: (r) => `${r.trigger} · ${r.kind}` },
    { key: "summary", header: "Summary", className: styles.summary, cell: summary },
  ];
  return (
    <>
      <DataTable
        dense
        detailInset
        label="Runs"
        columns={columns}
        rows={mirror.runs}
        rowKey={(r) => r.id}
        rowActions={(r) => (
          <Button size="small" aria-expanded={open.has(r.id)} onClick={() => onToggle(r.id)}>
            {open.has(r.id) ? "Hide log " : "Log "}
            <span className="visually-hidden">of the run at {minuteText(r.started_at)}</span>
          </Button>
        )}
        rowDetail={(r) => (open.has(r.id) ? <RunLog output={r.output} /> : null)}
      />
      <p className={styles.note}>The 10 newest runs, with the tools&apos; output.</p>
    </>
  );
}

/** The tools' output, plain text, scrolling, with a "Wrap lines" toggle. */
export function RunLog({ output }: { output: string }) {
  const [wrap, setWrap] = useState(false);
  if (!output) return <span className={styles.dim}>No output.</span>;
  return (
    <div className={styles.log}>
      <Button size="small" aria-pressed={wrap} onClick={() => setWrap((w) => !w)}>
        Wrap lines
      </Button>
      <pre className={wrap ? styles.wrap : undefined}>{output}</pre>
    </div>
  );
}

function Dash() {
  return <span className={styles.dim}>—</span>;
}
