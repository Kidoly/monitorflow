"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";

import { rich, t } from "@/i18n";
import {
  ApiError,
  type ServerUpdateCheck,
  type ServerUpdateRun,
  type ServerUpdateView,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { BACKUPS_PATH } from "@/lib/routes";
import { changeLines, imageText, inProgress, isFinal, restoreCommand, STATE_TONES, stateWord } from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import { ReleaseImport } from "./ReleaseImport";
import styles from "./ServerUpdate.module.css";
import { Button } from "./ui/Button";
import { FactsBox } from "./ui/FactsBox";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import { formatClock } from "./ui/time";

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

  const header = (
    <PageHeader
      title={t("serverUpdate.title")}
      meta={
        <>
          {t("serverUpdate.meta")}
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
        <Check view={data} busy={busy} onStarted={refresh} />
      )}
      {data.update && <LastUpdate run={data.update} requested={data.requested} loadedAt={loadedAt} />}
      {!data.update && data.requested && <Banner tone="info">{t("serverUpdate.waiting")}</Banner>}
      <ReleaseImport source={data.source} />
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

/** "Check for updates", then what the latest release changes and "Update". */
function Check({ view, busy, onStarted }: { view: ServerUpdateView; busy: boolean; onStarted: () => void }) {
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
  const available = view.available?.version;
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

  async function start(version: string) {
    await api.startServerUpdate(version);
    setResult(null);
    onStarted();
  }

  const latest = result?.latest ?? null;
  const ready = view.updater?.alive === true && !busy;
  return (
    <section className={styles.box} aria-labelledby="check-title">
      <div className={styles.boxHeader}>
        <h2 id="check-title" className={styles.title}>
          {t("serverUpdate.updates")}
        </h2>
        <Button onClick={() => void check()} disabled={checking}>
          {checking ? t("serverUpdate.checking") : t("serverUpdate.check")}
        </Button>
      </div>
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

function LastUpdate({ run, requested, loadedAt }: { run: ServerUpdateRun; requested: boolean; loadedAt: Date }) {
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
      {run.state === "succeeded" && (
        <Banner tone="ok">
          {t("serverUpdate.last.runs", { version: run.to })}
          {reload && (
            <>
              {" "}
              {rich("serverUpdate.last.reload", {
                button: (
                  <button type="button" className={styles.link} onClick={() => window.location.reload()}>
                    {t("serverUpdate.last.reloadButton")}
                  </button>
                ),
              })}
            </>
          )}
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
      <ol className={styles.steps} aria-label={t("serverUpdate.last.steps")}>
        {run.steps.map((step, i) => (
          <li key={i}>
            <time dateTime={step.at} className={styles.dim}>
              {formatClock(new Date(step.at))}
            </time>{" "}
            {step.message}
          </li>
        ))}
      </ol>
    </section>
  );
}
