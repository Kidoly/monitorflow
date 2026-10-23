"use client";

import { usePathname, useSearchParams } from "next/navigation";
import { useCallback, useState } from "react";

import { ApiError, type Mirror, type MirrorRunSummary, api, describeError } from "@/lib/api";
import { administersEverything, useMe } from "@/lib/me";
import { describeMirror, intervalText, nextSync, shortTime } from "@/lib/mirrors";
import { mirrorHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Mirrors.module.css";
import { Button } from "./ui/Button";
import { type Column, DataTable, PrimaryCell, useUrlSort } from "./ui/DataTable";
import { FilterBar } from "./ui/FilterBar";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine, StaleBanner } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { StatTiles } from "./ui/StatTiles";
import { Label, StatusLabel } from "./ui/StatusLabel";

type RunFilter = MirrorRunSummary["state"] | "never";

const runOf = (m: Mirror): RunFilter => m.last_run?.state ?? "never";

/** Failed, running, never run, then succeeded; by name within each. */
const RANK: Record<RunFilter, number> = { failed: 0, running: 1, never: 2, succeeded: 3 };
const problemsFirst = (a: Mirror, b: Mirror) => RANK[runOf(a)] - RANK[runOf(b)] || a.name.localeCompare(b.name);

/** The last run as a status word and when, or "Never run". */
export function LastRun({ run, now }: { run: MirrorRunSummary | null; now: Date }) {
  if (!run) return <Label tone="neutral">Never run</Label>;
  return (
    <>
      <StatusLabel kind="run" value={run.state} />{" "}
      <span className={styles.dim}>
        {run.state === "running" ? (
          `since ${shortTime(new Date(run.started_at), now)}`
        ) : (
          <RelativeTime value={run.finished_at ?? run.started_at} now={now} />
        )}
      </span>
    </>
  );
}

/** "20:30", "due", "manual", or "every 12 h" when the last sync is not known. */
export function nextSyncText(
  mirror: Mirror,
  lastSync: MirrorRunSummary | null | undefined,
  now: Date,
): string {
  if (mirror.sync_interval_s === null) return "manual";
  const running = mirror.last_run?.state === "running";
  if (running) return "—";
  const next = nextSync(mirror, lastSync, running);
  if (!next) return intervalText(mirror.sync_interval_s);
  return next <= now ? "due" : shortTime(next, now);
}

/** In a list, only a last run that is a sync tells when the last sync started. */
const lastSyncOf = (m: Mirror) => (m.last_run === null ? null : m.last_run.kind === "sync" ? m.last_run : undefined);

/**
 * Starting syncs and snapshots: one request at a time from a page, the
 * error of the last one, and a refresh on success.
 */
export function useMirrorJobs(refresh: () => void) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const start = useCallback(
    async (mirror: Mirror, kind: "sync" | "snapshot") => {
      setBusy(mirror.id);
      setError(null);
      try {
        await (kind === "sync" ? api.syncMirror(mirror.id) : api.snapshotMirror(mirror.id));
        refresh();
      } catch (e) {
        const what = kind === "sync" ? "a sync" : "a snapshot";
        setError(
          e instanceof ApiError && e.status === 409
            ? `Cannot start ${what} of ${mirror.name}: ${e.message}.`
            : `Cannot start ${what} of ${mirror.name}. ${describeError(e)}`,
        );
      } finally {
        setBusy(null);
      }
    },
    [refresh],
  );
  return { busy, error, start };
}

/** `/admin/mirrors/`: global administrators only, like the API. */
export function MirrorListPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Mirrors need a global administrator">
        Mirrors are shared by every customer. Ask a global administrator for a role.
      </NoPermission>
    );
  }
  return <MirrorList />;
}

function MirrorList() {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(api.mirrors);
  const jobs = useMirrorJobs(refresh);
  const params = useSearchParams();
  const pathname = usePathname();
  const [sort, setSort] = useUrlSort();

  const meta = (
    <>
      Global administrators only
      {(updatedAt !== null || error !== null) && (
        <>
          {" · "}
          <RefreshLine updatedAt={updatedAt} stale={stale} />
        </>
      )}
    </>
  );
  const header = <PageHeader title="Mirrors" meta={meta} />;
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title="Mirrors need a global administrator" />
      </div>
    );
  }
  if (!data || !updatedAt) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="mirrors" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const now = updatedAt;
  const run = params.get("run") as RunFilter | null;
  const family = params.get("family");
  const search = (params.get("q") ?? "").trim().toLowerCase();
  const rows = data.mirrors.filter(
    (m) =>
      (!run || runOf(m) === run) && (!family || m.family === family) && (!search || m.name.includes(search)),
  );
  const count = (state: RunFilter) => data.mirrors.filter((m) => runOf(m) === state).length;
  const tile = (label: string, state: RunFilter, tone: "ok" | "info" | "bad" | "neutral") => {
    const next = new URLSearchParams(params);
    if (run === state) next.delete("run");
    else next.set("run", state);
    const query = next.toString();
    return { label, value: count(state), tone, href: query ? `${pathname}?${query}` : pathname, active: run === state };
  };

  const columns: Column<Mirror>[] = [
    {
      key: "name",
      header: "Mirror",
      className: styles.wrap,
      sortValue: (m) => m.name,
      cell: (m) => <PrimaryCell title={m.name} href={mirrorHref(m.id)} sub={describeMirror(m)} />,
    },
    { key: "run", header: "Last run", sortValue: (m) => RANK[runOf(m)], cell: (m) => <LastRun run={m.last_run} now={now} /> },
    {
      key: "distribution",
      header: "Distribution",
      cell: (m) => m.distribution || <span className={styles.dim}>—</span>,
    },
    {
      key: "snapshot",
      header: "Current snapshot",
      sortValue: (m) => m.current_snapshot,
      cell: (m) => m.current_snapshot ?? <span className={styles.dim}>none</span>,
    },
    {
      key: "next",
      header: "Next sync",
      className: styles.dim,
      cell: (m) => nextSyncText(m, lastSyncOf(m), now),
    },
  ];

  return (
    <div className={styles.page}>
      {header}
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {jobs.error && <Banner tone="bad">{jobs.error}</Banner>}
      <StatTiles
        dense
        maxWidth={1000}
        tiles={[
          tile("Succeeded", "succeeded", "ok"),
          tile("Running", "running", "info"),
          tile("Failed", "failed", "bad"),
          tile("Never run", "never", "neutral"),
        ]}
      />
      <FilterBar
        label="Mirror filters"
        filters={[
          {
            name: "family",
            label: "Family",
            options: [
              { value: "apt", label: "apt" },
              { value: "dnf", label: "dnf" },
            ],
          },
          {
            name: "run",
            label: "Last run",
            options: [
              { value: "succeeded", label: "Succeeded" },
              { value: "running", label: "Running" },
              { value: "failed", label: "Failed" },
              { value: "never", label: "Never run" },
            ],
          },
        ]}
        search={{ name: "q", label: "Search", placeholder: "Mirror name", width: 200 }}
      />
      {data.mirrors.length === 0 ? (
        <EmptyState title="No mirrors yet">
          Add one on the server host with <code>monitorflow-server mirror add</code>.
        </EmptyState>
      ) : rows.length === 0 ? (
        <EmptyState>No mirror matches these filters.</EmptyState>
      ) : (
        <DataTable
          dense
          label="Mirrors"
          columns={columns}
          rows={rows}
          rowKey={(m) => m.id}
          sort={sort}
          onSortChange={setSort}
          defaultOrder={problemsFirst}
          rowActions={(m) => <MirrorButtons mirror={m} jobs={jobs} />}
        />
      )}
      <p className={styles.note}>Open a mirror for its snapshots and run history.</p>
    </div>
  );
}

/** Sync now and Snapshot of a row, disabled while a run of the mirror is going. */
function MirrorButtons({ mirror, jobs }: { mirror: Mirror; jobs: ReturnType<typeof useMirrorJobs> }) {
  const disabled = jobs.busy !== null || mirror.last_run?.state === "running";
  return (
    <>
      <Button size="small" disabled={disabled} onClick={() => void jobs.start(mirror, "sync")}>
        Sync now <span className="visually-hidden">{mirror.name}</span>
      </Button>
      <Button size="small" disabled={disabled} onClick={() => void jobs.start(mirror, "snapshot")}>
        Snapshot <span className="visually-hidden">{mirror.name}</span>
      </Button>
    </>
  );
}
