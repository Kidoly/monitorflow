"use client";

import { useCallback, useState } from "react";

import { rich, t } from "@/i18n";
import { ApiError, type BackupFile, type BackupKind, type BackupsView, api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { backupDownloadHref } from "@/lib/routes";
import { restoreCommand } from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./ServerUpdate.module.css";
import { Button, buttonClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { FactsBox } from "./ui/FactsBox";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine } from "./ui/PageState";
import { missingText } from "./ui/Missing";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

/** Polling while a backup runs or waits. */
const ACTIVE_MS = 3_000;
/** Older than this, the last good backup is worth a warning even without a failure. */
const LATE_MS = 2 * 24 * 60 * 60 * 1000;

const kindWord = (kind: BackupKind) => t(`backups.kind.${kind}`);

/** `/settings/backups/`: global administrators only, like the API (ADR-0058). */
export function BackupsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("backups.noPermission")}>
        {multiCustomer(me) ? t("backups.noPermissionNote") : t("backups.noPermissionNoteOne")}
      </NoPermission>
    );
  }
  return <Backups />;
}

/** One line for where backups stand, and its tint. */
export function backupSummary(view: BackupsView, now: Date): [string, Tone] {
  const last = view.last;
  if (view.requested || last?.state === "running") return [t("backups.summary.running"), "info"];
  if (last?.state === "failed") {
    return [t("backups.summary.failed", { error: last.error ?? t("backups.summary.unknownError") }), "bad"];
  }
  if (!view.last_success_at) return [t("backups.summary.none"), "neutral"];
  if (now.getTime() - new Date(view.last_success_at).getTime() > LATE_MS) {
    return [t("backups.summary.late"), "warn"];
  }
  return [t("backups.summary.ok"), "ok"];
}

function Backups() {
  const me = useMe();
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
      setFailure(e instanceof ApiError && e.status < 500 ? t("backups.cannot", { message: e.message }) : describeError(e));
    } finally {
      setStarting(false);
    }
  }

  const header = (
    <PageHeader
      title={t("backups.title")}
      meta={
        <>
          {t("backups.meta")}
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
        <NoPermission title={t("backups.noPermission")} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("backups.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const now = updatedAt ?? new Date();
  const busy = data.requested || data.last?.state === "running";
  const [summary, tone] = backupSummary(data, now);
  const backUpButton = (
    <Button variant="k" onClick={() => void backUpNow()} disabled={starting || busy || !data.updater_alive}>
      {busy ? t("backups.backingUp") : t("backups.backUpNow")}
    </Button>
  );
  return (
    <div className={styles.page}>
      {header}
      {!data.enabled ? (
        <Banner tone="warn">
          {t("backups.disabled")}
        </Banner>
      ) : (
        !data.updater_alive && (
          <Banner tone="bad">
            {rich("backups.updaterDown", {
              command: <code>docker compose -f deploy/docker-compose.yml up -d updater</code>,
            })}
          </Banner>
        )
      )}
      <Banner tone="warn">
        {multiCustomer(me) ? t("backups.secret") : t("backups.secretOne")}
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
              label: t("backups.facts.last"),
              value: data.last_success_at ? (
                <RelativeTime value={data.last_success_at} now={now} />
              ) : (
                t("backups.facts.noneYet")
              ),
            },
            { label: t("backups.facts.size"), value: data.files[0] ? formatBytes(data.files[0].size) : missingText("none") },
            {
              label: t("backups.facts.next"),
              value: data.next_at ? <RelativeTime value={data.next_at} now={now} /> : t("backups.facts.notScheduled"),
            },
            { label: t("backups.facts.kept"), value: t("backups.facts.keptValue", { count: data.keep }) },
          ]}
        />
      </section>
      <Files view={data} now={now} action={data.enabled ? backUpButton : null} />
      <section className={styles.box} aria-labelledby="restore-title">
        <h2 id="restore-title" className={styles.title}>
          {t("backups.restore.title")}
        </h2>
        <p className={styles.note}>{t("backups.restore.how")}</p>
        <pre className={styles.commands}>{restoreCommand(data.files[0]?.name ?? t("backups.restore.file"))}</pre>
        <p className={styles.note}>{t("backups.restore.after")}</p>
      </section>
    </div>
  );
}

function Files({ view, now, action }: { view: BackupsView; now: Date; action: React.ReactNode }) {
  if (view.files.length === 0) {
    return (
      <EmptyState title={t("backups.empty.title")} actions={action}>
        {t("backups.empty.note")}
      </EmptyState>
    );
  }
  const columns: Column<BackupFile>[] = [
    { key: "created", header: t("backups.column.taken"), cell: (f) => <RelativeTime value={f.created_at} now={now} /> },
    { key: "kind", header: t("backups.column.kind"), cell: (f) => kindWord(f.kind) },
    { key: "size", header: t("backups.column.size"), cell: (f) => formatBytes(f.size) },
  ];
  return (
    <DataTable
      label={t("backups.title")}
      columns={columns}
      rows={view.files}
      rowKey={(f) => f.name}
      rowActions={(f) =>
        view.downloads ? (
          <a className={buttonClass({ size: "small" })} href={backupDownloadHref(f.name)} download={f.name}>
            {t("backups.download")}
          </a>
        ) : view.host_dir ? (
          <span className={styles.dim}>{t("backups.inHostDir")}</span>
        ) : (
          <span className={styles.dim}>{t("backups.needsCompose")}</span>
        )
      }
    />
  );
}
