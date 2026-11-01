"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type ReactNode, useCallback, useEffect, useState } from "react";

import { ApiError, type MirrorDetail, type MirrorRun, type MirrorSnapshot, api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { administersEverything, useMe } from "@/lib/me";
import { describeMirror, durationText, intervalText, minuteText, retentionText } from "@/lib/mirrors";
import { MIRRORS_PATH, campaignHref, serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import { RemovalLabel, nextSyncText, useMirrorJobs } from "./MirrorList";
import styles from "./Mirrors.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, ButtonLink } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, NotFound, PageHeader, StaleBanner } from "./ui/PageState";
import { StatTiles } from "./ui/StatTiles";
import { Label, StatusLabel, statusWord } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

const SNAPSHOT_STATES: Record<MirrorSnapshot["state"], [string, Tone]> = {
  creating: ["Creating", "info"],
  published: ["Published", "ok"],
  failed: ["Failed", "bad"],
  pruning: ["Being removed", "info"],
  pruned: ["Removed", "neutral"],
};

const KEPT: Record<NonNullable<MirrorSnapshot["kept"]>, string> = {
  current: "The current snapshot",
  campaign: "An update campaign is pinned to it",
  server: "A server uses it",
  relay: "A relay uses it",
  newest: "Among the newest",
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

const removed = (
  <NotFound title="Mirror removed" actions={<ButtonLink href={MIRRORS_PATH}>Back to mirrors</ButtonLink>}>
    Its packages are gone from this server and the disk space is free. Relays delete their copy at their next sync.
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

  if (!valid || (error instanceof ApiError && error.status === 404)) return data?.deleting_at ? removed : notFound;
  if (!data || !updatedAt) {
    return error ? <ErrorState what="the mirror" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />;
  }

  const mirror = data;
  const running = mirror.last_run?.state === "running";
  const [word, tone] = LAST_RUN[mirror.last_run?.state ?? "never"];
  const lastSync = mirror.runs.find((r) => r.kind === "sync") ?? (mirror.runs.length === 0 ? null : undefined);
  const disabled = jobs.busy !== null || running || mirror.deleting_at !== null;
  const toggle = (runId: string) =>
    setOpen((current) => {
      const next = new Set(current);
      if (!next.delete(runId)) next.add(runId);
      return next;
    });

  // A snapshot needs a sync first, and a mirror publishing its own needs nothing else.
  const snapshotNext = mirror.runs.length > 0 && !mirror.auto_snapshot;
  const emptyAction = (
    <Button variant="k" disabled={disabled} onClick={() => void jobs.start(mirror, snapshotNext ? "snapshot" : "sync")}>
      {snapshotNext ? "Create snapshot" : "Sync now"}
    </Button>
  );

  return (
    <div className={styles.page}>
      <Breadcrumb parents={[{ label: "Mirrors", href: MIRRORS_PATH }]} current={mirror.name} />
      <PageHeader
        detail
        title={mirror.name}
        status={mirror.deleting_at ? <RemovalLabel mirror={mirror} /> : <Label tone={tone}>{word}</Label>}
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
      {mirror.deleting_at && <Removing mirror={mirror} onRetried={refresh} />}
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
      <Snapshots mirror={mirror} action={emptyAction} />
      <Runs mirror={mirror} open={open} onToggle={toggle} action={emptyAction} />
      {!mirror.deleting_at && <RemoveMirror mirror={mirror} onRemoving={refresh} />}
    </div>
  );
}

/** What a refusal to remove a mirror says, in the server's words when it has a reason. */
function notRemoved(error: unknown): string {
  return error instanceof ApiError && error.status === 409 ? `Not removed: ${error.message}.` : `Not removed. ${describeError(error)}`;
}

/** The way to remove a mirror, or what has to happen first. */
function RemoveMirror({ mirror, onRemoving }: { mirror: MirrorDetail; onRemoving: () => void }) {
  const { campaigns, servers } = mirror.used_by;
  if (campaigns.length > 0 || servers.length > 0) {
    return (
      <section className={styles.inUse} aria-label="Remove this mirror">
        <p>
          <strong>{mirror.name} can&apos;t be removed yet</strong>, because it is in use.
        </p>
        {campaigns.length > 0 && (
          <>
            <p>Wait until these update campaigns finish, or stop them:</p>
            <ul>
              {campaigns.map((c) => (
                <li key={c.id}>
                  <Link href={campaignHref(c.id)}>{c.name}</Link>
                </li>
              ))}
            </ul>
          </>
        )}
        {servers.length > 0 && (
          <>
            <p>Send these servers back to their own package sources, on their Updates tab:</p>
            <ul>
              {servers.map((s) => (
                <li key={s.id}>
                  <Link href={serverHref(s.id, "updates")}>{s.hostname}</Link>
                </li>
              ))}
            </ul>
          </>
        )}
      </section>
    );
  }
  const stopped = mirror.last_run?.state === "running" ? "The run going now is stopped. " : "";
  return (
    <div>
      <InlineConfirm
        trigger={`Remove ${mirror.name}…`}
        title={`Remove ${mirror.name}?`}
        consequence={
          `${stopped}Its snapshots and packages are deleted from this server, and the disk space comes back. ` +
          "Relays delete their copy at their next sync. This can't be undone: adding the mirror again downloads everything again."
        }
        confirmLabel={`Remove ${mirror.name}`}
        pendingLabel="Removing…"
        onConfirm={async () => {
          await api.deleteMirror(mirror.id);
          onRemoving();
        }}
        describeError={notRemoved}
      />
    </div>
  );
}

/** Where a removal stands, with the way to try again after a failure. */
function Removing({ mirror, onRetried }: { mirror: MirrorDetail; onRetried: () => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const retry = async () => {
    setBusy(true);
    setError(null);
    try {
      await api.deleteMirror(mirror.id);
      onRetried();
    } catch (e) {
      setError(notRemoved(e));
    } finally {
      setBusy(false);
    }
  };
  if (!mirror.delete_error) {
    return (
      <Banner tone="info" role="status">
        Removing this mirror. Nothing can use it any more. It finishes by itself once the syncs running now have ended, so
        you can leave this page.
      </Banner>
    );
  }
  return (
    <Banner tone="bad">
      The mirror could not be fully removed: {mirror.delete_error}. MonitorFlow tries again every hour.{" "}
      {error && `${error} `}
      <Button size="small" disabled={busy} onClick={() => void retry()}>
        Try again now
      </Button>
    </Banner>
  );
}

function Snapshots({ mirror, action }: { mirror: MirrorDetail; action: ReactNode }) {
  if (mirror.snapshots.length === 0) {
    return (
      <EmptyState title="No snapshots yet" actions={action}>
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
    {
      key: "kept",
      header: "Kept because",
      cell: (s) => {
        if (s.kept) return KEPT[s.kept];
        return s.state === "published" ? <span className={styles.dim}>Removed at the next cleanup</span> : <Dash />;
      },
    },
  ];
  return (
    <>
      <DataTable dense label="Snapshots" columns={columns} rows={mirror.snapshots} rowKey={(s) => s.id} />
      <p className={styles.note}>{retentionText(mirror.keep_snapshots)}</p>
    </>
  );
}

function Runs({
  mirror,
  open,
  onToggle,
  action,
}: {
  mirror: MirrorDetail;
  open: Set<string>;
  onToggle: (id: string) => void;
  action: ReactNode;
}) {
  if (mirror.runs.length === 0) {
    // Without snapshots either, the empty state above has the same button.
    return mirror.snapshots.length === 0 ? null : <EmptyState actions={action}>No runs yet.</EmptyState>;
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
