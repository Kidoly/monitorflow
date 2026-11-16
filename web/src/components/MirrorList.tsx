"use client";

import { usePathname, useSearchParams } from "next/navigation";
import { useCallback, useState } from "react";

import { t } from "@/i18n";
import { ApiError, type Mirror, type MirrorRunSummary, api, describeError } from "@/lib/api";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { describeMirror, intervalText, nextSync, retentionText, shortTime } from "@/lib/mirrors";
import { mirrorHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { AddMirror } from "./AddMirror";
import styles from "./Mirrors.module.css";
import { Button } from "./ui/Button";
import { type Column, DataTable, PrimaryCell, useUrlSort } from "./ui/DataTable";
import { FilterBar, NoMatches } from "./ui/FilterBar";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine, StaleBanner } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { StatTiles } from "./ui/StatTiles";
import { Label, StatusLabel } from "./ui/StatusLabel";

type RunFilter = MirrorRunSummary["state"] | "never";

const runOf = (m: Mirror): RunFilter => m.last_run?.state ?? "never";

/** Failed, running, never run, then succeeded; by name within each. */
const RANK: Record<RunFilter, number> = { failed: 0, running: 1, never: 2, succeeded: 3 };
const problemsFirst = (a: Mirror, b: Mirror) => RANK[runOf(a)] - RANK[runOf(b)] || a.name.localeCompare(b.name);

/** A mirror being removed, in the place of its last run. */
export function RemovalLabel({ mirror }: { mirror: Mirror }) {
  return mirror.delete_error ? (
    <Label tone="bad">{t("mirrors.removalFailed")}</Label>
  ) : (
    <Label tone="info">{t("mirrors.beingRemoved")}</Label>
  );
}

/** The last run as a status word and when, or "Never run". */
export function LastRun({ run, now }: { run: MirrorRunSummary | null; now: Date }) {
  if (!run) return <Label tone="neutral">{t("mirrors.neverRun")}</Label>;
  return (
    <>
      <StatusLabel kind="run" value={run.state} />{" "}
      <span className={styles.dim}>
        {run.state === "running" ? (
          t("mirrors.since", { time: shortTime(new Date(run.started_at), now) })
        ) : (
          <RelativeTime value={run.finished_at ?? run.started_at} now={now} />
        )}
      </span>
    </>
  );
}

/** "20:30", "due", "manual", "after this one" during a sync, or "every 12 h" when the last sync is not known. */
export function nextSyncText(
  mirror: Mirror,
  lastSync: MirrorRunSummary | null | undefined,
  now: Date,
): string {
  if (mirror.sync_interval_s === null) return t("mirrors.manual");
  const running = mirror.last_run?.state === "running";
  if (running) return t("mirrors.afterThisOne");
  const next = nextSync(mirror, lastSync, running);
  if (!next) return intervalText(mirror.sync_interval_s);
  return next <= now ? t("mirrors.due") : shortTime(next, now);
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
        const name = mirror.name;
        setError(
          e instanceof ApiError && e.status === 409
            ? t(kind === "sync" ? "mirrors.start.sync" : "mirrors.start.snapshot", { name, reason: e.message })
            : t(kind === "sync" ? "mirrors.start.syncFailed" : "mirrors.start.snapshotFailed", {
                name,
                error: describeError(e),
              }),
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
      <NoPermission title={t("mirrors.role.title")}>
        {multiCustomer(me) ? t("mirrors.role.everyCustomer") : t("mirrors.role.allSites")}
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
  const [adding, setAdding] = useState(false);
  const [added, setAdded] = useState<string | null>(null);

  const meta = (updatedAt !== null || error !== null) && (
    <RefreshLine updatedAt={updatedAt} stale={stale} />
  );
  const openAdd = () => {
    setAdded(null);
    setAdding(true);
  };
  const addButton = (
    <Button variant="k" onClick={openAdd}>
      {t("mirrors.add")}
    </Button>
  );
  const header = <PageHeader title={t("mirrors.title")} meta={meta} actions={!adding && addButton} />;
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("mirrors.role.title")} />
      </div>
    );
  }
  if (!data || !updatedAt) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("mirrors.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
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
      header: t("mirrors.column.mirror"),
      className: styles.wrap,
      sortValue: (m) => m.name,
      cell: (m) => <PrimaryCell title={m.name} href={mirrorHref(m.id)} sub={describeMirror(m)} />,
    },
    {
      key: "run",
      header: t("mirrors.column.lastRun"),
      sortValue: (m) => RANK[runOf(m)],
      cell: (m) => (m.deleting_at ? <RemovalLabel mirror={m} /> : <LastRun run={m.last_run} now={now} />),
    },
    {
      key: "distribution",
      header: t("mirrors.column.distribution"),
      cell: (m) => m.distribution,
    },
    {
      key: "snapshot",
      header: t("mirrors.column.snapshot"),
      sortValue: (m) => m.current_snapshot,
      cell: (m) => m.current_snapshot ?? <span className={styles.dim}>{t("mirrors.none")}</span>,
    },
    {
      key: "next",
      header: t("mirrors.column.nextSync"),
      className: styles.dim,
      cell: (m) => nextSyncText(m, lastSyncOf(m), now),
    },
  ];

  return (
    <div className={styles.page}>
      {header}
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {jobs.error && <Banner tone="bad">{jobs.error}</Banner>}
      {added && (
        <Banner tone="ok" role="status">
          {t("mirrors.added", { name: added })}
        </Banner>
      )}
      {adding && (
        <AddMirror
          onClose={() => setAdding(false)}
          onAdded={(name) => {
            setAdded(name);
            setAdding(false);
            refresh();
          }}
        />
      )}
      <StatTiles
        dense
        maxWidth={1000}
        tiles={[
          tile(t("mirrors.run.succeeded"), "succeeded", "ok"),
          tile(t("mirrors.run.running"), "running", "info"),
          tile(t("mirrors.run.failed"), "failed", "bad"),
          tile(t("mirrors.neverRun"), "never", "neutral"),
        ]}
      />
      <FilterBar
        label={t("mirrors.filters")}
        filters={[
          {
            name: "family",
            label: t("mirrors.family"),
            options: [
              { value: "apt", label: "apt" },
              { value: "dnf", label: "dnf" },
            ],
          },
          {
            name: "run",
            label: t("mirrors.column.lastRun"),
            options: [
              { value: "succeeded", label: t("mirrors.run.succeeded") },
              { value: "running", label: t("mirrors.run.running") },
              { value: "failed", label: t("mirrors.run.failed") },
              { value: "never", label: t("mirrors.neverRun") },
            ],
          },
        ]}
        search={{ name: "q", label: t("mirrors.search"), placeholder: t("mirrors.searchPlaceholder"), width: 200 }}
      />
      {data.mirrors.length === 0 ? (
        <EmptyState title={t("mirrors.empty.title")} actions={!adding && addButton}>
          {t("mirrors.empty.text")}
        </EmptyState>
      ) : rows.length === 0 ? (
        <NoMatches names={["family", "run", "q"]}>{t("mirrors.noMatch")}</NoMatches>
      ) : (
        <DataTable
          dense
          label={t("mirrors.title")}
          columns={columns}
          rows={rows}
          rowKey={(m) => m.id}
          sort={sort}
          onSortChange={setSort}
          defaultOrder={problemsFirst}
          rowActions={(m) => <MirrorButtons mirror={m} jobs={jobs} />}
        />
      )}
      <p className={styles.note}>{t("mirrors.openHint")}</p>
      <p className={styles.note}>{retentionText(data.keep_snapshots)}</p>
    </div>
  );
}

/** Sync now and Snapshot of a row, disabled while a run of the mirror is going. */
function MirrorButtons({ mirror, jobs }: { mirror: Mirror; jobs: ReturnType<typeof useMirrorJobs> }) {
  const disabled = jobs.busy !== null || mirror.last_run?.state === "running" || mirror.deleting_at !== null;
  return (
    <>
      <Button size="small" disabled={disabled} onClick={() => void jobs.start(mirror, "sync")}>
        {t("mirrors.syncNow")} <span className="visually-hidden">{mirror.name}</span>
      </Button>
      <Button size="small" disabled={disabled} onClick={() => void jobs.start(mirror, "snapshot")}>
        {t("mirrors.snapshot")} <span className="visually-hidden">{mirror.name}</span>
      </Button>
    </>
  );
}
