"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { formatClock } from "@/components/ui/time";
import { type AcknowledgeLink, ApiError, LOGIN_PATH, api, describeError } from "@/lib/api";

import styles from "./LoginForm.module.css";

type State =
  | { kind: "checking" }
  | { kind: "gone" }
  | { kind: "ready"; link: AcknowledgeLink }
  | { kind: "done"; link: AcknowledgeLink; at: Date };

const hhmm = (iso: string) => formatClock(new Date(iso)).slice(0, 5);

/**
 * `/acknowledge/#<token>` (ADR-0082): the link in a personal notification.
 * It shows what it acknowledges and as whom, and acknowledges only when the
 * button is pressed: opening the page, as a mail scanner does, changes
 * nothing.
 */
export function AcknowledgeForm() {
  const [state, setState] = useState<State>({ kind: "checking" });
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  // Read once: the effect strips the fragment, and may run twice in development.
  const secret = useRef<string | null>(null);

  useEffect(() => {
    if (secret.current === null) {
      secret.current = window.location.hash.slice(1);
      // Keep the secret out of the history and of anything copied from the address bar.
      if (secret.current) window.history.replaceState(null, "", window.location.pathname);
    }
    if (!secret.current) {
      setState({ kind: "gone" });
      return;
    }
    let current = true;
    api.inspectAcknowledgeLink(secret.current).then(
      (link) => current && setState({ kind: "ready", link }),
      (e) => {
        if (!current) return;
        if (e instanceof ApiError && e.status === 410) setState({ kind: "gone" });
        else setFailure(describeError(e));
      },
    );
    return () => {
      current = false;
    };
  }, []);

  async function acknowledge() {
    setPending(true);
    setFailure(null);
    try {
      const link = await api.useAcknowledgeLink(secret.current ?? "");
      setState({ kind: "done", link, at: new Date() });
    } catch (e) {
      if (e instanceof ApiError && e.status === 410) setState({ kind: "gone" });
      else if (e instanceof ApiError && e.status === 403) setFailure("Your account can no longer acknowledge these alerts.");
      else setFailure(describeError(e));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className={styles.page}>
      <main id="main" className={styles.main}>
        <h1 className={styles.brand}>MonitorFlow</h1>
        {state.kind === "gone" ? (
          <div className={styles.box}>
            <h2 className={styles.title}>Link no longer valid</h2>
            <Banner tone="warn">
              <strong>This link no longer works.</strong> It expired, or your account was disabled.
            </Banner>
            <p className={styles.text}>
              <Link href={LOGIN_PATH}>Sign in</Link> and open the alert to acknowledge it.
            </p>
          </div>
        ) : state.kind === "checking" ? (
          failure && <Banner tone="bad">{failure}</Banner>
        ) : (
          <div className={styles.box} aria-labelledby="acknowledge-title" role="region">
            <h2 id="acknowledge-title" className={styles.title}>
              {state.kind === "done" ? "Acknowledged" : `Acknowledge as ${state.link.username}`}
            </h2>
            {state.kind === "done" && (
              <Banner tone="ok">
                Acknowledged at {formatClock(state.at).slice(0, 5)}. Nobody else is told next.
              </Banner>
            )}
            <ul className={styles.text}>
              {state.link.incidents.map((i) => (
                <li key={i.id}>
                  {i.text}
                  {i.state === "resolved"
                    ? " (resolved)"
                    : i.acknowledged_by && i.acknowledged_at
                      ? ` (acknowledged by ${i.acknowledged_by} at ${hhmm(i.acknowledged_at)})`
                      : ""}
                </li>
              ))}
            </ul>
            {state.link.incidents.length === 0 && (
              <p className={styles.text}>Your account can no longer see these alerts.</p>
            )}
            {failure && <Banner tone="bad">{failure}</Banner>}
            {state.kind === "ready" && (
              <>
                <p className={styles.text}>Says you are on it: nobody else is told next.</p>
                <Button
                  className={styles.submit}
                  variant="k"
                  size="large"
                  onClick={acknowledge}
                  disabled={pending || state.link.incidents.length === 0}
                >
                  {pending ? "Acknowledging…" : "Acknowledge"}
                </Button>
              </>
            )}
          </div>
        )}
      </main>
    </div>
  );
}
