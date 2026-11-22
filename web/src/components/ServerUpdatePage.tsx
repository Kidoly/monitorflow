"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";

import { rich, t } from "@/i18n";
import {
  ApiError,
  type ServerUpdateCheck,
  type ServerUpdateHistoryEntry,
  type ServerUpdateResult,
  type ServerUpdateRun,
  type ServerUpdateView,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { BACKUPS_PATH } from "@/lib/routes";
import {
  changeLines,
  imageText,
  inProgress,
  isFinal,
  type MilestoneMark,
  milestoneMarks,
  milestoneWord,
  restoreCommand,
  STATE_TONES,
  stateWord,
} from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import { ReleaseImport } from "./ReleaseImport";
import styles from "./ServerUpdate.module.css";
import { Button } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { FactsBox } from "./ui/FactsBox";
import { Hint } from "./ui/Hint";
import { Missing } from "./ui/Missing";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import { formatClock } from "./ui/time";
import type { Tone } from "./ui/tone";

/** Polling while an update runs: the restart is short, the page follows it closely. */
const ACTIVE_MS = 3_000;
const GUIDE = "docs/deploy-server.md";

/** `/settings/server-update/`: global administrators only, like the API. */
export function ServerUpdatePage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("serverUpdate.noPermission")}>
        {multiCustomer(me) ? t("serverUpdate.noPermissionCustomers") : t("serverUpdate.noPermissionSites")}
      </NoPermission>
    );
  }
  return <ServerUpdate />;
}

function ServerUpdate() {
  const [active, setActive] = useState(false);
  const fetcher = useCallback(
    async (signal: AbortSignal) => {
      const view = await api.serverUpdate(signal);
      setActive(inProgress(view.update, view.requested));
      return view;
    },
    [],
  );
  const intervalMs = active ? ACTIVE_MS : undefined;
  const { data, error, stale, updatedAt, refresh } = usePolledData(fetcher, { intervalMs });
  // An update that ends after the page loaded may have replaced the web interface.
  const [loadedAt] = useState(() => new Date());
  const check = useUpdateCheck(data?.available?.version);

  const header = (
    <PageHeader
      title={t("serverUpdate.title")}
      meta={updatedAt !== null || error !== null ? <RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={intervalMs} /> : undefined}
      actions={
        data?.enabled && (
          <Button onClick={() => void check.run()} disabled={check.checking}>
            {check.checking ? t("serverUpdate.checking") : t("serverUpdate.check")}
          </Button>
        )
      }
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("serverUpdate.noPermission")} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("serverUpdate.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const busy = inProgress(data.update, data.requested);
  return (
    <div className={styles.page}>
      {header}
      {stale &&
        (busy ? (
          <Banner tone="warn">{t("serverUpdate.reconnecting")}</Banner>
        ) : (
          <Banner tone="warn">{t("serverUpdate.unreachable", { error: describeError(error) })}</Banner>
        ))}
      <Summary view={data} />
      {!data.enabled ? (
        <Banner tone="info">
          {data.available && <strong>{t("serverUpdate.available", { version: data.available.version })} </strong>}
          {rich("serverUpdate.notSetUp", { guide: <code>{GUIDE}</code> })}
        </Banner>
      ) : (
        <CheckResult view={data} check={check} busy={busy} onStarted={refresh} />
      )}
      {data.update && <LastUpdate run={data.update} view={data} loadedAt={loadedAt} />}
      {!data.update && data.requested && <Banner tone="info">{t("serverUpdate.waiting")}</Banner>}
      <UpdateHistory key={data.update?.state ?? ""} />
      <ReleaseImport source={data.source} />
      {data.update && data.update.steps.length > 0 && <RawLog run={data.update} />}
    </div>
  );
}

function Summary({ view }: { view: ServerUpdateView }) {
  const updater = view.updater;
  const running = updater?.running ?? null;
  return (
    <FactsBox
      columns={2}
      facts={[
        { label: t("serverUpdate.facts.running"), value: view.running_version },
        {
          label: t("serverUpdate.facts.updater"),
          value: !view.enabled ? (
            t("serverUpdate.facts.notConfigured")
          ) : updater === null ? (
            <Label tone="bad">{t("serverUpdate.facts.notRunning")}</Label>
          ) : updater.alive ? (
            <Label tone="ok">{t("serverUpdate.facts.ready")}</Label>
          ) : (
            <>
              <Label tone="bad">{t("serverUpdate.facts.notResponding")}</Label>{" "}
              {rich("serverUpdate.facts.lastSeen", { when: <RelativeTime value={updater.heartbeat_at} now={new Date()} /> })}
            </>
          ),
        },
        { label: t("serverUpdate.facts.serverImage"), value: imageText(running?.server_image ?? null), mono: true },
        { label: t("serverUpdate.facts.webImage"), value: imageText(running?.web_image ?? null), mono: true },
        { label: t("serverUpdate.facts.source"), value: view.source, mono: true },
      ]}
    />
  );
}

/** "Check for updates" (a button of the page header) and what it found. */
function useUpdateCheck(available: string | undefined) {
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<ServerUpdateCheck | null>(null);
  const [failure, setFailure] = useState<string | null>(null);

  async function check() {
    setChecking(true);
    setFailure(null);
    try {
      setResult(await api.checkServerUpdate());
    } catch (e) {
      setResult(null);
      setFailure(t("serverUpdate.cannotCheck", { error: describeError(e) }));
    } finally {
      setChecking(false);
    }
  }

  // The daily check saw a release: go straight to what it changes and
  // "Update". A failure here says nothing; "Check for updates" would.
  useEffect(() => {
    if (!available) return;
    let current = true;
    api.checkServerUpdate().then(
      (checked) => {
        if (current) setResult(checked);
      },
      () => {},
    );
    return () => {
      current = false;
    };
  }, [available]);

  return { checking, result, failure, run: check, clear: () => setResult(null) };
}

/** What "Check for updates" found: the latest release, what it changes, and "Update". */
function CheckResult({
  view,
  check,
  busy,
  onStarted,
}: {
  view: ServerUpdateView;
  check: ReturnType<typeof useUpdateCheck>;
  busy: boolean;
  onStarted: () => void;
}) {
  const { result, failure } = check;
  async function start(version: string) {
    await api.startServerUpdate(version);
    check.clear();
    onStarted();
  }

  const latest = result?.latest ?? null;
  const ready = view.updater?.alive === true && !busy;
  if (!result && !failure) return null;
  return (
    <section className={styles.box} aria-label={t("serverUpdate.updates")}>
      {failure && <Banner tone="bad">{failure}</Banner>}
      {result && !result.reachable && (
        <p className={styles.note}>{rich("serverUpdate.offline", { guide: <code>{GUIDE}</code> })}</p>
      )}
      {result?.reachable && !latest && (
        <Banner tone="bad">{t("serverUpdate.unusable", { error: result.error ?? "" })}</Banner>
      )}
      {latest && !latest.newer && <p className={styles.note}>{t("serverUpdate.upToDate", { version: view.running_version })}</p>}
      {latest?.newer && (
        <div className={styles.release}>
          <p>
            <strong>{t("serverUpdate.available", { version: latest.version })}</strong>{" "}
            {latest.release_notes_url && (
              <a href={latest.release_notes_url} target="_blank" rel="noreferrer">
                {t("serverUpdate.releaseNotes")}
              </a>
            )}
          </p>
          <ul className={styles.changes} aria-label={t("serverUpdate.whatChanges")}>
            {changeLines(result!).map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
          {ready ? (
            <InlineConfirm
              trigger={t("serverUpdate.update.trigger", { version: latest.version })}
              triggerVariant="k"
              title={t("serverUpdate.update.title", { version: latest.version })}
              consequence={t("serverUpdate.update.consequence")}
              confirmLabel={t("serverUpdate.update.confirm", { version: latest.version })}
              pendingLabel={t("serverUpdate.starting")}
              onConfirm={() => start(latest.version)}
              describeError={(e) => (e instanceof ApiError && e.status < 500 ? `${e.message}.` : describeError(e))}
            />
          ) : (
            <p className={styles.note}>
              {busy ? t("serverUpdate.inProgressNote") : t("serverUpdate.updaterDown")}
            </p>
          )}
        </div>
      )}
    </section>
  );
}

function LastUpdate({ run, view, loadedAt }: { run: ServerUpdateRun; view: ServerUpdateView; loadedAt: Date }) {
  const requested = view.requested;
  const word = stateWord(run.state);
  const tone = STATE_TONES[run.state];
  const now = new Date();
  const reload = run.state === "succeeded" && run.finished_at !== null && new Date(run.finished_at) > loadedAt;
  return (
    <section className={styles.box} aria-labelledby="last-title">
      <div className={styles.boxHeader}>
        <h2 id="last-title" className={styles.title}>
          {isFinal(run.state) ? t("serverUpdate.last.title") : t("serverUpdate.last.inProgress")}
        </h2>
        <Label tone={tone}>{word}</Label>
      </div>
      <p className={styles.note}>
        {rich("serverUpdate.last.askedBy", {
          from: run.from ?? "?",
          to: run.to,
          who: run.requested_by || t("serverUpdate.last.theUpdater"),
          when: <RelativeTime value={run.started_at} now={now} />,
        })}
        {run.backup && (
          <>
            {" · "}
            {rich("serverUpdate.last.backup", { file: <code>{run.backup}</code> })}
          </>
        )}
      </p>
      {/* The version is the facts' and the heading's to tell; the box is only for a reload. */}
      {reload && (
        <Banner tone="ok">
          {rich("serverUpdate.last.reload", {
            button: (
              <button type="button" className={styles.link} onClick={() => window.location.reload()}>
                {t("serverUpdate.last.reloadButton")}
              </button>
            ),
          })}
        </Banner>
      )}
      {run.state === "failed" && <Banner tone="bad">{t("serverUpdate.last.nothingChanged", { error: run.error ?? "" })}</Banner>}
      {run.state === "rolled_back" && !run.migrated && (
        <Banner tone="bad">
          {t("serverUpdate.last.runsAgain", { version: run.from ?? t("serverUpdate.last.previous"), error: run.error ?? "" })}
        </Banner>
      )}
      {run.state === "rolled_back" && run.migrated && (
        <Banner tone="bad">
          <p>
            {rich("serverUpdate.last.migrated", {
              error: run.error,
              link: <Link href={BACKUPS_PATH}>{t("serverUpdate.last.backups")}</Link>,
            })}
          </p>
          <pre className={styles.commands}>{restoreCommand(run.backup ?? t("serverUpdate.last.backupFile"))}</pre>
          <p>{t("serverUpdate.last.lost")}</p>
        </Banner>
      )}
      {requested && <p className={styles.note}>{t("serverUpdate.last.another")}</p>}
      {view.milestones.length > 0 && (
        <ol className={styles.milestones} aria-label={t("serverUpdate.last.steps")}>
          {milestoneMarks(view.milestones, run).map((m) => (
            <li key={m.kind} className={MARK_CLASSES[m.mark]}>
              <span className={styles.mark} aria-hidden="true">
                {MARK_ICONS[m.mark]}
              </span>
              <span>{milestoneWord(m.kind)}</span>
              <span className="visually-hidden"> ({t(`serverUpdate.mark.${m.mark}`)})</span>
              {m.at && (
                <time dateTime={m.at} className={styles.dim}>
                  {formatClock(new Date(m.at))}
                </time>
              )}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

const MARK_ICONS: Record<MilestoneMark, string> = { done: "✓", current: "…", failed: "✕", todo: "○" };
const MARK_CLASSES: Record<MilestoneMark, string | undefined> = {
  done: styles.done,
  current: styles.current,
  failed: styles.failed,
  todo: styles.todo,
};

/**
 * The updater's own lines, untranslated, closed at the bottom of the page:
 * for support, the milestones above are what a person follows.
 */
function RawLog({ run }: { run: ServerUpdateRun }) {
  return (
    <details className={styles.rawLog}>
      <summary>{t("serverUpdate.last.rawLog")}</summary>
      <p className={styles.note}>{t("serverUpdate.last.rawLogNote")}</p>
      <ol className={styles.steps} aria-label={t("serverUpdate.last.rawLog")}>
        {run.steps.map((step, i) => (
          <li key={i}>
            <time dateTime={step.at} className={styles.dim}>
              {formatClock(new Date(step.at))}
            </time>{" "}
            {step.message}
          </li>
        ))}
      </ol>
    </details>
  );
}

const RESULT_TONES: Record<ServerUpdateResult, Tone> = {
  in_progress: "info",
  succeeded: "ok",
  failed: "bad",
  rolled_back: "bad",
  unknown: "neutral",
};

/**
 * The server's updates, newest first (ADR-0172). Read again when the last
 * update changes state, through its `key`.
 */
function UpdateHistory() {
  const { data, error, refresh } = usePolledData(api.serverUpdateHistory);
  let body;
  if (!data) {
    body = error ? (
      <Banner tone="bad">
        <strong>{t("ui.page.loadFailed", { what: t("serverUpdate.history.what") })}</strong> {describeError(error)}{" "}
        <Button size="small" onClick={refresh}>
          {t("ui.page.retry")}
        </Button>
      </Banner>
    ) : (
      <LoadingBlock />
    );
  } else if (data.updates.length === 0) {
    body = <p className={styles.note}>{t("serverUpdate.history.empty")}</p>;
  } else {
    const now = new Date();
    const columns: Column<ServerUpdateHistoryEntry>[] = [
      {
        key: "version",
        header: t("serverUpdate.history.column.version"),
        cell: (u) => (
          <>
            {u.from && <span className={styles.dim}>{u.from} → </span>}
            <strong>{u.to}</strong>
          </>
        ),
      },
      {
        key: "date",
        header: t("serverUpdate.history.column.date"),
        cell: (u) => <RelativeTime value={u.started_at} now={now} />,
      },
      {
        key: "who",
        header: t("serverUpdate.history.column.who"),
        // Null for an update from the command line, or a user since removed.
        cell: (u) => u.requested_by ?? <Missing word="unknown" className={styles.dim} />,
      },
      {
        key: "result",
        header: t("serverUpdate.history.column.result"),
        cell: (u) => (
          <>
            {u.inferred ? (
              <Hint tone={RESULT_TONES[u.result]} label={t(`serverUpdate.history.result.${u.result}`)}>
                {t("serverUpdate.history.inferred")}
              </Hint>
            ) : (
              <Label tone={RESULT_TONES[u.result]}>{t(`serverUpdate.history.result.${u.result}`)}</Label>
            )}
            {/* The updater's words, as on the last update. */}
            {u.error && <span className={styles.dim}> {u.error}</span>}
          </>
        ),
      },
    ];
    body = (
      <DataTable
        bare
        label={t("serverUpdate.history.title")}
        columns={columns}
        rows={data.updates}
        rowKey={(u) => u.id}
      />
    );
  }
  return (
    <section className={styles.box} aria-labelledby="history-title">
      <h2 id="history-title" className={styles.title}>
        {t("serverUpdate.history.title")}
      </h2>
      {body}
    </section>
  );
}
