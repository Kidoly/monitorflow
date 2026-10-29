"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import {
  ApiError,
  type ServerUpdateCheck,
  type ServerUpdateRun,
  type ServerUpdateView,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, useMe } from "@/lib/me";
import { BACKUPS_PATH } from "@/lib/routes";
import { changeLines, imageText, inProgress, isFinal, restoreCommand, STATE_WORDS } from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./ServerUpdate.module.css";
import { Button } from "./ui/Button";
import { FactsBox } from "./ui/FactsBox";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";

/** Polling while an update runs: the restart is short, the page follows it closely. */
const ACTIVE_MS = 3_000;
const GUIDE = "docs/deploy-server.md";

/** `/admin/server-update/`: global administrators only, like the API. */
export function ServerUpdatePage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Server updates need a global administrator">
        The server is shared by every customer. Ask a global administrator for a role.
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
      title="Server update"
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
        <NoPermission title="Server updates need a global administrator" />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="the update status" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const busy = inProgress(data.update, data.requested);
  return (
    <div className={styles.page}>
      {header}
      {stale &&
        (busy ? (
          <Banner tone="warn">
            Reconnecting… The server and the web interface restart during an update. This page keeps asking and shows the
            result once they are back.
          </Banner>
        ) : (
          <Banner tone="warn">Cannot reach the server: {describeError(error)} This page keeps trying.</Banner>
        ))}
      <Summary view={data} />
      {!data.enabled ? (
        <Banner tone="info">
          This server is not set up for updates from the web interface: it needs the updater service of the production
          compose file. See <code>{GUIDE}</code>, Updating.
        </Banner>
      ) : (
        <Check view={data} busy={busy} onStarted={refresh} />
      )}
      {data.update && <LastUpdate run={data.update} requested={data.requested} loadedAt={loadedAt} />}
      {!data.update && data.requested && <Banner tone="info">Waiting for the updater to start the update…</Banner>}
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
        { label: "Running version", value: view.running_version },
        {
          label: "Updater",
          value: !view.enabled ? (
            "not configured"
          ) : updater === null ? (
            <Label tone="bad">not running</Label>
          ) : updater.alive ? (
            <Label tone="ok">ready</Label>
          ) : (
            <>
              <Label tone="bad">not responding</Label> last seen <RelativeTime value={updater.heartbeat_at} now={new Date()} />
            </>
          ),
        },
        { label: "Server image", value: imageText(running?.server_image ?? null), mono: true },
        { label: "Web image", value: imageText(running?.web_image ?? null), mono: true },
        { label: "Releases from", value: view.source, mono: true },
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
      setFailure(`Cannot check for updates. ${describeError(e)}`);
    } finally {
      setChecking(false);
    }
  }

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
          Updates
        </h2>
        <Button onClick={() => void check()} disabled={checking}>
          {checking ? "Checking…" : "Check for updates"}
        </Button>
      </div>
      {failure && <Banner tone="bad">{failure}</Banner>}
      {result && !result.reachable && (
        <Banner tone="warn">
          No update can be checked: {result.error}. A server without Internet access is updated by hand, from the release
          files and the saved images (<code>{GUIDE}</code>, Offline servers).
        </Banner>
      )}
      {result?.reachable && !latest && (
        <Banner tone="bad">The latest release cannot be used: {result.error}</Banner>
      )}
      {latest && !latest.newer && <p className={styles.note}>MonitorFlow {view.running_version} is up to date.</p>}
      {latest?.newer && (
        <div className={styles.release}>
          <p>
            <strong>MonitorFlow {latest.version} is available.</strong>{" "}
            {latest.release_notes_url && (
              <a href={latest.release_notes_url} target="_blank" rel="noreferrer">
                Release notes
              </a>
            )}
          </p>
          <ul className={styles.changes} aria-label="What changes">
            {changeLines(result!).map((line) => (
              <li key={line}>{line}</li>
            ))}
          </ul>
          {ready ? (
            <InlineConfirm
              trigger={`Update to ${latest.version}…`}
              triggerVariant="k"
              title={`Update the server to ${latest.version}?`}
              consequence={
                <>
                  The database and the server data are backed up first. The server and the web interface then restart, for a few minutes:
                  agents reconnect by themselves and this page follows the update. If the new version does not become
                  healthy, the previous one is restored.
                </>
              }
              confirmLabel={`Update to ${latest.version}`}
              pendingLabel="Starting…"
              onConfirm={() => start(latest.version)}
              describeError={(e) => (e instanceof ApiError && e.status < 500 ? `${e.message}.` : describeError(e))}
            />
          ) : (
            <p className={styles.note}>
              {busy ? "An update is in progress." : "The updater is not running, so the update cannot start."}
            </p>
          )}
        </div>
      )}
    </section>
  );
}

function LastUpdate({ run, requested, loadedAt }: { run: ServerUpdateRun; requested: boolean; loadedAt: Date }) {
  const [word, tone] = STATE_WORDS[run.state];
  const now = new Date();
  const reload = run.state === "succeeded" && run.finished_at !== null && new Date(run.finished_at) > loadedAt;
  return (
    <section className={styles.box} aria-labelledby="last-title">
      <div className={styles.boxHeader}>
        <h2 id="last-title" className={styles.title}>
          {isFinal(run.state) ? "Last update" : "Update in progress"}
        </h2>
        <Label tone={tone}>{word}</Label>
      </div>
      <p className={styles.note}>
        {run.from ?? "?"} → {run.to}, asked by {run.requested_by || "the updater"}{" "}
        <RelativeTime value={run.started_at} now={now} />
        {run.backup && (
          <>
            {" "}
            · backup <code>{run.backup}</code>
          </>
        )}
      </p>
      {run.state === "succeeded" && (
        <Banner tone="ok">
          The server runs {run.to}.
          {reload && (
            <>
              {" "}
              <button type="button" className={styles.link} onClick={() => window.location.reload()}>
                Reload the page
              </button>{" "}
              to use the new web interface.
            </>
          )}
        </Banner>
      )}
      {run.state === "failed" && <Banner tone="bad">Nothing was changed: {run.error}</Banner>}
      {run.state === "rolled_back" && !run.migrated && (
        <Banner tone="bad">
          {run.from ?? "The previous version"} runs again with the same data: {run.error}
        </Banner>
      )}
      {run.state === "rolled_back" && run.migrated && (
        <Banner tone="bad">
          <p>
            {run.error}. The new version had already migrated the database, and migrations are forward-only: the previous
            version cannot use it. Restore the backup taken before the update: download it from{" "}
            <Link href={BACKUPS_PATH}>Backups</Link>, then run on the server host:
          </p>
          <pre className={styles.commands}>{restoreCommand(run.backup ?? "<backup>")}</pre>
          <p>Data written since the backup is lost.</p>
        </Banner>
      )}
      {requested && <p className={styles.note}>Another update is waiting for the updater.</p>}
      <ol className={styles.steps} aria-label="Steps">
        {run.steps.map((step, i) => (
          <li key={i}>
            <time dateTime={step.at} className={styles.dim}>
              {new Date(step.at).toLocaleTimeString()}
            </time>{" "}
            {step.message}
          </li>
        ))}
      </ol>
    </section>
  );
}
