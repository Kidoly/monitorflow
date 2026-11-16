"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { formatClock } from "@/components/ui/time";
import { rich, t } from "@/i18n";
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
      else if (e instanceof ApiError && e.status === 403) setFailure(t("incidents.link.noLonger"));
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
            <h2 className={styles.title}>{t("incidents.link.goneTitle")}</h2>
            <Banner tone="warn">
              <strong>{t("incidents.link.gone")}</strong> {t("incidents.link.goneWhy")}
            </Banner>
            <p className={styles.text}>
              {rich("incidents.link.signIn", { link: <Link href={LOGIN_PATH}>{t("incidents.link.signInLink")}</Link> })}
            </p>
          </div>
        ) : state.kind === "checking" ? (
          failure && <Banner tone="bad">{failure}</Banner>
        ) : (
          <div className={styles.box} aria-labelledby="acknowledge-title" role="region">
            <h2 id="acknowledge-title" className={styles.title}>
              {state.kind === "done" ? t("incidents.link.done") : t("incidents.link.as", { name: state.link.username })}
            </h2>
            {state.kind === "done" && (
              <Banner tone="ok">
                {t("incidents.link.doneAt", { time: formatClock(state.at).slice(0, 5) })}
              </Banner>
            )}
            <ul className={styles.text}>
              {state.link.incidents.map((i) => (
                <li key={i.id}>
                  {i.text}
                  {i.state === "resolved"
                    ? t("incidents.link.resolved")
                    : i.acknowledged_by && i.acknowledged_at
                      ? t("incidents.link.acknowledgedBy", { name: i.acknowledged_by, time: hhmm(i.acknowledged_at) })
                      : ""}
                </li>
              ))}
            </ul>
            {state.link.incidents.length === 0 && (
              <p className={styles.text}>{t("incidents.link.cannotSee")}</p>
            )}
            {failure && <Banner tone="bad">{failure}</Banner>}
            {state.kind === "ready" && (
              <>
                <p className={styles.text}>{t("incidents.ack.says")}</p>
                <Button
                  className={styles.submit}
                  variant="k"
                  size="large"
                  onClick={acknowledge}
                  disabled={pending || state.link.incidents.length === 0}
                >
                  {pending ? t("incidents.button.busy") : t("incidents.button.acknowledge")}
                </Button>
              </>
            )}
          </div>
        )}
      </main>
    </div>
  );
}
