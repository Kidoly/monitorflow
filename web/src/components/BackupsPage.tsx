"use client";

import { useCallback, useState } from "react";

import { ApiError, type BackupFile, type BackupKind, type BackupsView, api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { administersEverything, useMe } from "@/lib/me";
import { backupDownloadHref } from "@/lib/routes";
import { restoreCommand } from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./ServerUpdate.module.css";
import { Button, buttonClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { FactsBox } from "./ui/FactsBox";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

/** Polling while a backup runs or waits. */
const ACTIVE_MS = 3_000;
/** Older than this, the last good backup is worth a warning even without a failure. */
const LATE_MS = 2 * 24 * 60 * 60 * 1000;

const KIND_WORDS: Record<BackupKind, string> = {
  daily: "Daily",
  manual: "Manual",
  update: "Before an update",
};

/** `/admin/backups/`: global administrators only, like the API (ADR-0058). */
export function BackupsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Backups need a global administrator">
        A backup holds every customer&apos;s data and the server&apos;s keys. Ask a global administrator for a role.
      </NoPermission>
    );
  }
  return <Backups />;
}

/** One line for where backups stand, and its tint. */
export function backupSummary(view: BackupsView, now: Date): [string, Tone] {
  const last = view.last;
  if (view.requested || last?.state === "running") return ["Backing up…", "info"];
  if (last?.state === "failed") return [`The last backup failed: ${last.error ?? "unknown error"}`, "bad"];
  if (!view.last_success_at) return ["No backup yet.", "neutral"];
  if (now.getTime() - new Date(view.last_success_at).getTime() > LATE_MS) {
    return ["The last backup is more than two days old.", "warn"];
  }
  return ["Backed up.", "ok"];
}

function Backups() {
  const [active, setActive] = useState(false);
  const fetcher = useCallback(async (signal: AbortSignal) => {
    const view = await api.backups(signal);
    setActive(view.requested || view.last?.state === "running");
    return view;
  }, []);
  const intervalMs = active ? ACTIVE_MS : undefined;
  const { data, error, stale, updatedAt, refresh } = usePolledData(fetcher, { intervalMs });
  const [starting, setStarting] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  async function backUpNow() {
    setStarting(true);
    setFailure(null);
    try {
      await api.startBackup();
      setActive(true);
      refresh();
    } catch (e) {
      setFailure(e instanceof ApiError && e.status < 500 ? `Cannot back up: ${e.message}.` : describeError(e));
    } finally {
      setStarting(false);
    }
  }

  const header = (
    <PageHeader
      title="Backups"
      meta={
        <>
          Global administrators only
          {(updatedAt !== null || error !== null) && (
            <>
              {" · "}
              <RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={intervalMs} />
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
        <NoPermission title="Backups need a global administrator" />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="the backups" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const now = updatedAt ?? new Date();
  const busy = data.requested || data.last?.state === "running";
  const [summary, tone] = backupSummary(data, now);
  const backUpButton = (
    <Button variant="k" onClick={() => void backUpNow()} disabled={starting || busy || !data.updater_alive}>
      {busy ? "Backing up…" : "Back up now"}
    </Button>
  );
  return (
    <div className={styles.page}>
      {header}
      {!data.enabled ? (
        <Banner tone="warn">
          This server takes no backups: they need the updater service of the production compose file
          (deploy/docker-compose.yml).
        </Banner>
      ) : (
        !data.updater_alive && (
          <Banner tone="bad">
            The updater is not running, so no backup is taken. On the server host:{" "}
            <code>docker compose -f deploy/docker-compose.yml up -d updater</code>
          </Banner>
        )
      )}
      <Banner tone="warn">
        A backup holds the database and the server&apos;s certificate authority and keys. Anyone with the file can read
        every customer&apos;s data and pose as this server: keep downloaded copies somewhere safe.
      </Banner>
      <section className={styles.box} aria-labelledby="status-title">
        <div className={styles.boxHeader}>
          <h2 id="status-title" className={styles.title}>
            <Label tone={tone}>{summary}</Label>
          </h2>
          {data.enabled && backUpButton}
        </div>
        {failure && <Banner tone="bad">{failure}</Banner>}
        {data.last?.state === "succeeded" && data.last.warning && <Banner tone="warn">{data.last.warning}</Banner>}
        <FactsBox
          columns={2}
          facts={[
            {
              label: "Last backup",
              value: data.last_success_at ? <RelativeTime value={data.last_success_at} now={now} /> : "none yet",
            },
            { label: "Size", value: data.files[0] ? formatBytes(data.files[0].size) : "—" },
            {
              label: "Next automatic backup",
              value: data.next_at ? <RelativeTime value={data.next_at} now={now} /> : "—",
            },
            { label: "Kept", value: `the ${data.keep} most recent, every day and before each update` },
          ]}
        />
      </section>
      <Files view={data} now={now} action={data.enabled ? backUpButton : null} />
      <section className={styles.box} aria-labelledby="restore-title">
        <h2 id="restore-title" className={styles.title}>
          Restore
        </h2>
        <p className={styles.note}>
          Download a backup, copy it to the server host, and run there, on this server or a fresh install:
        </p>
        <pre className={styles.commands}>{restoreCommand(data.files[0]?.name ?? "<backup file>")}</pre>
        <p className={styles.note}>
          It replaces the database and the server&apos;s keys, then restarts MonitorFlow; agents reconnect by
          themselves. Data written after the backup is lost. Metrics, logs and mirror contents are not in backups.
        </p>
      </section>
    </div>
  );
}

function Files({ view, now, action }: { view: BackupsView; now: Date; action: React.ReactNode }) {
  if (view.files.length === 0) {
    return (
      <EmptyState title="No backup yet" actions={action}>
        The first one is taken a few minutes after the server starts, then every day.
      </EmptyState>
    );
  }
  const columns: Column<BackupFile>[] = [
    { key: "created", header: "Taken", cell: (f) => <RelativeTime value={f.created_at} now={now} /> },
    { key: "kind", header: "Kind", cell: (f) => KIND_WORDS[f.kind] },
    { key: "size", header: "Size", cell: (f) => formatBytes(f.size) },
  ];
  return (
    <DataTable
      label="Backups"
      columns={columns}
      rows={view.files}
      rowKey={(f) => f.name}
      rowActions={(f) =>
        view.downloads ? (
          <a className={buttonClass({ size: "small" })} href={backupDownloadHref(f.name)} download={f.name}>
            Download
          </a>
        ) : (
          <span className={styles.dim}>Download needs the current compose file</span>
        )
      }
    />
  );
}
