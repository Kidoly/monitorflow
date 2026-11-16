"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useState, useSyncExternalStore } from "react";

import { SetupHint } from "@/components/SetupHint";
import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { t } from "@/i18n";
import { ApiError, api, describeError } from "@/lib/api";

import styles from "./LoginForm.module.css";

/** Seconds between reachability checks after the server did not answer. */
export const RETRY_SECONDS = 5;

type Failure =
  | { kind: "credentials" }
  /** `sent`: the login request itself failed, so the credentials may have left. */
  | { kind: "unreachable"; sent: boolean }
  | { kind: "other"; message: string };

const unreachable = (e: unknown) => e instanceof ApiError && e.status === 0;

// The host is only known in the browser; the static HTML renders without it.
const noSubscription = () => () => {};
const useHost = () => useSyncExternalStore(noSubscription, () => window.location.host, () => "");

export function LoginForm() {
  const router = useRouter();
  const host = useHost();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [retryIn, setRetryIn] = useState(RETRY_SECONDS);
  const [setupPending, setSetupPending] = useState(false);
  const waiting = failure?.kind === "unreachable";

  // Without any account, sign-in cannot work: say how to create the first one (ADR-0071).
  useEffect(() => {
    const controller = new AbortController();
    api.setupStatus(controller.signal).then(
      (status) => setSetupPending(status.pending),
      () => {},
    );
    return () => controller.abort();
  }, []);

  // While unreachable, check the server every few seconds; sign-in waits for it.
  useEffect(() => {
    if (!waiting) return;
    let left = RETRY_SECONDS;
    const timer = setInterval(() => {
      left -= 1;
      if (left > 0) {
        setRetryIn(left);
        return;
      }
      left = RETRY_SECONDS;
      setRetryIn(left);
      api.health().then(
        () => setFailure(null),
        () => {},
      );
    }, 1000);
    return () => clearInterval(timer);
  }, [waiting]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setPending(true);
    setFailure(null);
    setRetryIn(RETRY_SECONDS);
    try {
      // A dead server is found before the credentials are posted.
      await api.health();
    } catch (e) {
      setFailure(unreachable(e) ? { kind: "unreachable", sent: false } : { kind: "other", message: describeError(e) });
      setPending(false);
      return;
    }
    try {
      await api.login(username, password);
      router.replace("/");
    } catch (e) {
      setFailure(
        e instanceof ApiError && e.status === 401
          ? { kind: "credentials" }
          : unreachable(e)
            ? { kind: "unreachable", sent: true }
            : { kind: "other", message: describeError(e) },
      );
      if (e instanceof ApiError && e.status === 401) setPassword("");
      setPending(false);
    }
  }

  const invalid = failure?.kind === "credentials";
  const server = host || t("auth.theServer");

  if (setupPending) {
    return (
      <div className={styles.page}>
        <main id="main" className={styles.main}>
          <h1 className={styles.brand}>MonitorFlow</h1>
          <div className={styles.box}>
            <h2 className={styles.title}>{t("auth.setupPending.title")}</h2>
            <Banner tone="warn">
              <strong>{t("auth.setupPending.noAccount")}</strong> {t("auth.setupPending.text")}
            </Banner>
            <SetupHint />
          </div>
        </main>
      </div>
    );
  }

  return (
    <div className={styles.page}>
      <main id="main" className={styles.main}>
        <h1 className={styles.brand}>MonitorFlow</h1>
        <form className={styles.box} onSubmit={onSubmit} aria-labelledby="signin-title">
          <h2 id="signin-title" className={styles.title}>
            {t("auth.signIn")}
          </h2>
          {failure?.kind === "credentials" && (
            <Banner tone="bad">
              <strong>{t("auth.wrong.title")}</strong> {t("auth.wrong.text")}
            </Banner>
          )}
          {failure?.kind === "unreachable" && (
            // An alert, not a status: it stops the sign-in.
            <Banner tone="warn" role="alert">
              <strong>{t("auth.unreachable.title")}</strong> {t("auth.unreachable.text", { server })}
              {!failure.sent && ` ${t("auth.unreachable.notSent")}`}
            </Banner>
          )}
          {failure?.kind === "other" && <Banner tone="bad">{failure.message}</Banner>}
          <label className={styles.label}>
            {t("auth.username")}
            <input
              className={styles.input}
              name="username"
              autoComplete="username"
              autoCapitalize="none"
              spellCheck={false}
              required
              aria-invalid={invalid || undefined}
              value={username}
              onChange={(e) => setUsername(e.target.value)}
            />
          </label>
          <label className={styles.label}>
            {t("auth.password")}
            <input
              className={styles.input}
              name="password"
              type="password"
              autoComplete="current-password"
              required
              aria-invalid={invalid || undefined}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
            />
          </label>
          <Button className={styles.submit} variant="k" size="large" type="submit" disabled={pending || waiting}>
            {pending ? t("auth.signingIn") : waiting ? t("auth.retryingIn", { seconds: String(retryIn) }) : t("auth.signIn")}
          </Button>
        </form>
        {host && <p className={styles.foot}>{host} · MonitorFlow</p>}
      </main>
    </div>
  );
}
