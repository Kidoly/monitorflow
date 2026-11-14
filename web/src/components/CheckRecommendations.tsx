"use client";

import { useCallback, useId, useState } from "react";

import { ApiError, type CheckRecommendation, type ProposedCheck, api, describeError } from "@/lib/api";
import { CHECK_TYPES, checkRequests, checkTitle, saveThreshold } from "@/lib/alerts";
import { usePolledData } from "@/lib/usePolledData";
import { Button } from "@/components/ui/Button";
import { RelativeTime } from "@/components/ui/RelativeTime";

import admin from "./AdminPages.module.css";
import styles from "./CheckRecommendations.module.css";

/** "Service postgresql@16-main.service", "Port 5432": what Add creates. */
export function proposedTitle(check: ProposedCheck): string {
  return checkTitle({ origin: "server", entry_id: null, kind: check.kind, name: check.kind, instance: "", interval_ms: 0, timeout_ms: null, params: check.params });
}

/**
 * Adds the checks of `recommendation` to the server, each with the alert
 * that comes with it, as Add a check does. One that exists already is
 * left as it is.
 */
export async function addRecommended(serverId: string, recommendation: CheckRecommendation, intervalMs: number): Promise<void> {
  const owner = { kind: "servers" as const, id: serverId };
  for (const check of recommendation.checks) {
    const type = CHECK_TYPES.find((t) => t.kind === check.kind);
    if (!type) continue;
    const { sensor, threshold } = checkRequests(type, check.params, intervalMs);
    try {
      await api.createSensor(owner, sensor);
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 409)) throw e;
      continue;
    }
    if (threshold) await saveThreshold(owner, threshold);
  }
}

/**
 * What the agent saw running that nothing checks yet (ADR-0134), with
 * Add and Dismiss. Nothing is added without a click.
 */
export function CheckRecommendations({
  serverId,
  osFamily,
  intervalMs,
  canEdit,
  onAdded,
}: {
  serverId: string;
  osFamily: string;
  /** How often the checks it adds run: the site's interval. */
  intervalMs: number;
  canEdit: boolean;
  onAdded: () => void;
}) {
  const titleId = useId();
  const fetcher = useCallback((signal: AbortSignal) => api.recommendations(serverId, signal), [serverId]);
  const data = usePolledData(fetcher, { intervalMs: 60_000 });
  const [showDismissed, setShowDismissed] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [failure, setFailure] = useState<{ key: string; text: string } | null>(null);

  if (!data.data) return null;
  const { reported_at, recommendations } = data.data;
  const open = recommendations.filter((r) => r.dismissed_at === null);
  const dismissed = recommendations.filter((r) => r.dismissed_at !== null);

  async function act(key: string, what: () => Promise<void>, after: () => void) {
    setBusy(key);
    setFailure(null);
    try {
      await what();
      after();
      data.refresh();
    } catch (e) {
      setFailure({ key, text: `It was not changed. ${describeError(e)}` });
    } finally {
      setBusy(null);
    }
  }

  const source =
    reported_at === null ? (
      osFamily === "windows" ? (
        "Windows agents do not report what runs on their server yet."
      ) : (
        "The agent has not reported what runs on this server yet."
      )
    ) : (
      <>
        From what the agent saw, <RelativeTime value={reported_at} now={data.updatedAt ?? new Date()} />. It looks again every 5 minutes.
      </>
    );

  return (
    <section className={styles.box} aria-labelledby={titleId}>
      <div className={styles.head}>
        <h3 id={titleId} className={styles.title}>
          Recommended checks
        </h3>
        <span className={admin.dim}>{source}</span>
      </div>
      {reported_at !== null && open.length === 0 && (
        <p className={styles.none}>Nothing to add: what runs here is checked.</p>
      )}
      <ul className={styles.list}>
        {(showDismissed ? recommendations : open).map((r) => {
          const isDismissed = r.dismissed_at !== null;
          const count = r.checks.length;
          return (
            <li key={r.key} className={styles.item}>
              <div className={styles.text}>
                <span className={styles.name}>{r.title}</span>
                <span className={admin.dim}>{r.evidence.join(", ")}.</span>
                <span className={styles.adds}>
                  {isDismissed
                    ? `Dismissed${r.dismissed_by ? ` by ${r.dismissed_by}` : ""}.`
                    : `Adds: ${r.checks.map(proposedTitle).join(", ")}, ${count === 1 ? "with its alert" : "each with its alert"}.`}
                </span>
                {failure?.key === r.key && (
                  <span role="alert" className={admin.hintError}>
                    {failure.text}
                  </span>
                )}
              </div>
              {canEdit && (
                <span className={styles.actions}>
                  {isDismissed ? (
                    <Button
                      size="small"
                      disabled={busy !== null}
                      onClick={() => void act(r.key, () => api.restoreRecommendation(serverId, r.key), () => undefined)}
                    >
                      Bring back
                    </Button>
                  ) : (
                    <>
                      <Button
                        variant="k"
                        size="small"
                        disabled={busy !== null}
                        aria-label={`Add ${count === 1 ? "the check" : `${count} checks`} for ${r.title}`}
                        onClick={() => void act(r.key, () => addRecommended(serverId, r, intervalMs), onAdded)}
                      >
                        {busy === r.key ? "Adding…" : count === 1 ? "Add check" : `Add ${count} checks`}
                      </Button>
                      <Button
                        size="small"
                        disabled={busy !== null}
                        aria-label={`Dismiss ${r.title}`}
                        onClick={() => void act(r.key, () => api.dismissRecommendation(serverId, r.key), () => undefined)}
                      >
                        Dismiss
                      </Button>
                    </>
                  )}
                </span>
              )}
            </li>
          );
        })}
      </ul>
      {dismissed.length > 0 && (
        <Button size="small" className={styles.toggle} onClick={() => setShowDismissed((v) => !v)}>
          {showDismissed ? "Hide dismissed" : `Show dismissed (${dismissed.length})`}
        </Button>
      )}
    </section>
  );
}
