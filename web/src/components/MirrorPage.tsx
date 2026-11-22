"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type ReactNode, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import { ApiError, type MirrorDetail, type MirrorRun, type MirrorSnapshot, api, describeError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { describeMirror, durationText, intervalText, minuteText, retentionText, syncHint } from "@/lib/mirrors";
import { MIRRORS_PATH, campaignHref, serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import { RemovalLabel, nextSyncText, useMirrorJobs } from "./MirrorList";
import styles from "./Mirrors.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, ButtonLink } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { Missing } from "./ui/Missing";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, NotFound, PageHeader, StaleBanner } from "./ui/PageState";
import { StatTiles } from "./ui/StatTiles";
import { Label, StatusLabel, statusWord } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

const SNAPSHOT_TONES: Record<MirrorSnapshot["state"], Tone> = {
  creating: "info",
  published: "ok",
  failed: "bad",
  pruning: "info",
  pruned: "neutral",
};

const LAST_RUN_TONES: Record<MirrorRun["state"] | "never", Tone> = {
  running: "info",
  succeeded: "ok",
  failed: "bad",
  never: "neutral",
};

/** `/admin/mirrors/view/?id=…` (ADR-0020 routing). */
export function MirrorView() {
  const id = useSearchParams().get("id");
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("mirrors.role.title")}>
        {multiCustomer(me) ? t("mirrors.role.everyCustomer") : t("mirrors.role.allSites")}
      </NoPermission>
    );
  }
  return <MirrorPage key={id} id={id} />;
}

const notFound = () => (
  <NotFound
    title={t("mirrors.notFound")}
    actions={<ButtonLink href={MIRRORS_PATH}>{t("mirrors.backToMirrors")}</ButtonLink>}
  >
    {t("mirrors.notFoundText")}
  </NotFound>
);

const removed = () => (
  <NotFound title={t("mirrors.removed")} actions={<ButtonLink href={MIRRORS_PATH}>{t("mirrors.backToMirrors")}</ButtonLink>}>
    {t("mirrors.removedText")}
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

  if (!valid || (error instanceof ApiError && error.status === 404)) return data?.deleting_at ? removed() : notFound();
  if (!data || !updatedAt) {
    return error ? <ErrorState what={t("mirrors.theMirror")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />;
  }

  const mirror = data;
  const running = mirror.last_run?.state === "running";
  const lastRun = mirror.last_run?.state ?? "never";
  const tone = LAST_RUN_TONES[lastRun];
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
      {snapshotNext ? t("mirrors.createSnapshot") : t("mirrors.syncNow")}
    </Button>
  );

  return (
    <div className={styles.page}>
      <Breadcrumb parents={[{ label: t("mirrors.title"), href: MIRRORS_PATH }]} />
      <PageHeader
        detail
        title={mirror.name}
        status={mirror.deleting_at ? <RemovalLabel mirror={mirror} /> : undefined}
        meta={`${describeMirror(mirror, { distribution: true })} · ${intervalText(mirror.sync_interval_s)}`}
        actions={
          <>
            {/* Each sync of the others publishes a snapshot already. */}
            {!mirror.auto_snapshot && (
              <Button
                disabled={disabled}
                title={t("mirrors.createSnapshot.hint")}
                onClick={() => void jobs.start(mirror, "snapshot")}
              >
                {t("mirrors.createSnapshot")}
              </Button>
            )}
            <Button
              variant="k"
              disabled={disabled}
              title={syncHint(mirror)}
              onClick={() => void jobs.start(mirror, "sync")}
            >
              {t("mirrors.syncNow")}
            </Button>
          </>
        }
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {jobs.error && <Banner tone="bad">{jobs.error}</Banner>}
      {mirror.deleting_at && <Removing mirror={mirror} onRetried={refresh} />}
      <StatTiles
        dense
        label={t("mirrors.summary")}
        tiles={[
          {
            label: t("mirrors.tile.snapshots"),
            value: mirror.snapshots.filter((s) => s.state === "published").length,
            tone: "neutral",
          },
          {
            label: t("mirrors.tile.lastRun"),
            value: mirror.last_run ? statusWord("run", mirror.last_run.state)[0] : t("mirrors.tile.never"),
            tone,
          },
          { label: t("mirrors.tile.nextSync"), value: nextSyncText(mirror, lastSync, updatedAt), tone: "neutral" },
        ]}
      />
      {!mirror.deleting_at && <ModeLine mirror={mirror} onSwitched={refresh} />}
      <Snapshots mirror={mirror} action={emptyAction} />
      <Runs mirror={mirror} open={open} onToggle={toggle} action={emptyAction} />
      {!mirror.deleting_at && <RemoveMirror mirror={mirror} onRemoving={refresh} />}
    </div>
  );
}

/** The mode of the mirror, what it means, and the way to the other one (ADR-0142). */
function ModeLine({ mirror, onSwitched }: { mirror: MirrorDetail; onSwitched: () => void }) {
  const [switched, setSwitched] = useState(false);
  const onDemand = mirror.mode === "on_demand";
  const name = mirror.name;
  return (
    <section className={styles.modeLine} aria-label={t("mirrors.mode.label")}>
      <p>
        <strong>{t("mirrors.mode.current", { mode: t(`mirrors.mode.${mirror.mode}`) })}</strong>{" "}
        <span className={styles.dim}>{onDemand ? t("mirrors.mode.onDemandHelp") : t("mirrors.mode.fullHelp")}</span>
      </p>
      {switched && (
        <Banner tone="ok" role="status">
          {t("mirrors.mode.switched")}
        </Banner>
      )}
      <div>
        <InlineConfirm
          key={mirror.mode}
          trigger={onDemand ? t("mirrors.mode.switchToFull") : t("mirrors.mode.switchToOnDemand")}
          triggerVariant="default"
          title={onDemand ? t("mirrors.mode.toFullTitle", { name }) : t("mirrors.mode.toOnDemandTitle", { name })}
          consequence={onDemand ? t("mirrors.mode.toFull") : t("mirrors.mode.toOnDemand")}
          confirmLabel={onDemand ? t("mirrors.mode.confirmFull", { name }) : t("mirrors.mode.confirmOnDemand", { name })}
          confirmVariant="k"
          pendingLabel={t("mirrors.mode.switching")}
          onConfirm={async () => {
            await api.setMirrorMode(mirror.id, onDemand ? "full" : "on_demand");
            setSwitched(true);
            onSwitched();
          }}
          describeError={(e) => t("mirrors.mode.notSwitched", { error: describeError(e) })}
        />
      </div>
    </section>
  );
}

/** What a refusal to remove a mirror says, in the server's words when it has a reason. */
function notRemoved(error: unknown): string {
  return error instanceof ApiError && error.status === 409
    ? t("mirrors.notRemovedReason", { reason: error.message })
    : t("mirrors.notRemoved", { error: describeError(error) });
}

/** The way to remove a mirror, or what has to happen first. */
function RemoveMirror({ mirror, onRemoving }: { mirror: MirrorDetail; onRemoving: () => void }) {
  const { campaigns, servers } = mirror.used_by;
  if (campaigns.length > 0 || servers.length > 0) {
    return (
      <section className={styles.inUse} aria-label={t("mirrors.removeThis")}>
        <p>
          <strong>{t("mirrors.inUse", { name: mirror.name })}</strong>
          {t("mirrors.inUseWhy")}
        </p>
        {campaigns.length > 0 && (
          <>
            <p>{t("mirrors.waitCampaigns")}</p>
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
            <p>{t("mirrors.sendBack")}</p>
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
  const stopped = mirror.last_run?.state === "running" ? `${t("mirrors.runStopped")} ` : "";
  return (
    <div>
      <InlineConfirm
        trigger={t("mirrors.remove.trigger", { name: mirror.name })}
        title={t("mirrors.remove.title", { name: mirror.name })}
        consequence={`${stopped}${t("mirrors.remove.consequence")}`}
        confirmLabel={t("mirrors.remove.confirm", { name: mirror.name })}
        pendingLabel={t("mirrors.remove.pending")}
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
        {t("mirrors.removing")}
      </Banner>
    );
  }
  return (
    <Banner tone="bad">
      {t("mirrors.removeError", { error: mirror.delete_error })}{" "}
      {error && `${error} `}
      <Button size="small" disabled={busy} onClick={() => void retry()}>
        {t("mirrors.tryAgain")}
      </Button>
    </Banner>
  );
}

function Snapshots({ mirror, action }: { mirror: MirrorDetail; action: ReactNode }) {
  if (mirror.snapshots.length === 0) {
    return (
      <EmptyState title={t("mirrors.snapshots.empty")} actions={action}>
        {mirror.auto_snapshot ? t("mirrors.snapshots.auto") : t("mirrors.snapshots.manual")}
      </EmptyState>
    );
  }
  const columns: Column<MirrorSnapshot>[] = [
    { key: "name", header: t("mirrors.snapshot"), className: styles.strong, cell: (s) => s.name },
    {
      key: "state",
      header: t("mirrors.column.state"),
      cell: (s) => (
        <>
          <Label tone={SNAPSHOT_TONES[s.state]}>{t(`mirrors.snapshot.${s.state}`)}</Label>
          {s.state === "published" && s.name === mirror.current_snapshot && (
            <span className={styles.dim}> {t("mirrors.current")}</span>
          )}
          {s.error && <span className={styles.dim}> {s.error}</span>}
        </>
      ),
    },
    { key: "size", header: t("mirrors.column.size"), cell: (s) => (s.size_bytes === null ? <Missing word="unknown" className={styles.dim} /> : formatBytes(s.size_bytes)) },
    { key: "created", header: t("mirrors.column.created"), cell: (s) => minuteText(s.created_at) },
    {
      key: "published",
      header: t("mirrors.column.published"),
      cell: (s) => (s.published_at ? minuteText(s.published_at) : <Missing word="notYet" className={styles.dim} />),
    },
    {
      key: "kept",
      header: t("mirrors.column.kept"),
      cell: (s) => {
        if (s.kept) return t(`mirrors.kept.${s.kept}`);
        return s.state === "published" ? <span className={styles.dim}>{t("mirrors.nextCleanup")}</span> : null;
      },
    },
  ];
  return (
    <>
      <DataTable dense label={t("mirrors.snapshots")} columns={columns} rows={mirror.snapshots} rowKey={(s) => s.id} />
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
    return mirror.snapshots.length === 0 ? null : <EmptyState actions={action}>{t("mirrors.noRuns")}</EmptyState>;
  }
  const snapshotName = (id: string | null) => mirror.snapshots.find((s) => s.id === id)?.name;
  const summary = (run: MirrorRun) => {
    if (run.error) return run.error;
    if (run.state === "running") return run.kind === "sync" ? t("mirrors.syncing") : t("mirrors.creatingSnapshot");
    const name = snapshotName(run.snapshot_id);
    if (run.kind === "snapshot") {
      return name ? t("mirrors.snapshotPublished", { name }) : t("mirrors.snapshotPublishedNoName");
    }
    return name ? t("mirrors.synced", { name }) : t("mirrors.syncedNoName");
  };
  const columns: Column<MirrorRun>[] = [
    { key: "run", header: t("mirrors.column.run"), className: styles.strong, cell: (r) => minuteText(r.started_at) },
    { key: "state", header: t("mirrors.column.state"), cell: (r) => <StatusLabel kind="run" value={r.state} /> },
    {
      key: "duration",
      header: t("mirrors.column.duration"),
      cell: (r) =>
        r.finished_at ? (
          durationText(new Date(r.finished_at).getTime() - new Date(r.started_at).getTime())
        ) : (
          <Missing word="notYet" className={styles.dim} />
        ),
    },
    { key: "trigger", header: t("mirrors.column.trigger"), cell: (r) => `${t(`mirrors.trigger.${r.trigger}`)} · ${t(`mirrors.kind.${r.kind}`)}` },
    { key: "summary", header: t("mirrors.column.summary"), className: styles.summary, cell: summary },
  ];
  return (
    <DataTable
      dense
      detailInset
      label={t("mirrors.runs")}
      columns={columns}
      rows={mirror.runs}
      rowKey={(r) => r.id}
      rowActions={(r) => (
        <Button size="small" aria-expanded={open.has(r.id)} onClick={() => onToggle(r.id)}>
          {open.has(r.id) ? t("mirrors.hideLog") : t("mirrors.log")}{" "}
          <span className="visually-hidden">{t("mirrors.ofRun", { time: minuteText(r.started_at) })}</span>
        </Button>
      )}
      rowDetail={(r) => (open.has(r.id) ? <RunLog output={r.output} /> : null)}
    />
  );
}

/** The tools' output, plain text, scrolling, with a "Wrap lines" toggle. */
export function RunLog({ output }: { output: string }) {
  const [wrap, setWrap] = useState(false);
  if (!output) return <span className={styles.dim}>{t("mirrors.noOutput")}</span>;
  return (
    <div className={styles.log}>
      <Button size="small" aria-pressed={wrap} onClick={() => setWrap((w) => !w)}>
        {t("mirrors.wrap")}
      </Button>
      <pre className={wrap ? styles.wrap : undefined}>{output}</pre>
    </div>
  );
}
