"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useState, useSyncExternalStore } from "react";

import { SetupHint } from "@/components/SetupHint";
import { Button, buttonClass } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { Logo } from "@/components/ui/Logo";
import { t } from "@/i18n";
import { ApiError, type LoginProvider, api, describeError } from "@/lib/api";
import { oidcStartHref } from "@/lib/routes";

import styles from "./LoginForm.module.css";

/** Seconds between reachability checks after the server did not answer. */
export const RETRY_SECONDS = 5;

type Failure =
  | { kind: "credentials" }
  /** A directory took the password, but no group gives access (ADR-0145). */
  | { kind: "noAccess" }
  /** `sent`: the login request itself failed, so the credentials may have left. */
  | { kind: "unreachable"; sent: boolean }
  | { kind: "other"; message: string };

const unreachable = (e: unknown) => e instanceof ApiError && e.status === 0;

// The host is only known in the browser; the static HTML renders without it.
const noSubscription = () => () => {};
const useHost = () => useSyncExternalStore(noSubscription, () => window.location.host, () => "");

/** Why a sign-in through a provider came back here (`?sso=`), as the API names it. */
const SSO_REASONS = [
  "expired",
  "cancelled",
  "unavailable",
  "provider_error",
  "no_group",
  "username_taken",
  "disabled",
] as const;
type SsoReason = (typeof SSO_REASONS)[number];

function ssoReason(search: string): SsoReason | null {
  const reason = new URLSearchParams(search).get("sso");
  return SSO_REASONS.find((r) => r === reason) ?? null;
}

const useSsoReason = () =>
  useSyncExternalStore(noSubscription, () => ssoReason(window.location.search), () => null);

export function LoginForm() {
  const router = useRouter();
  const host = useHost();
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);
  const [retryIn, setRetryIn] = useState(RETRY_SECONDS);
  const [setupPending, setSetupPending] = useState(false);
  // null until known, so the form does not show and then fold away.
  const [providers, setProviders] = useState<LoginProvider[] | null>(null);
  const [showPassword, setShowPassword] = useState(false);
  const sso = useSsoReason();
  const waiting = failure?.kind === "unreachable";
  const buttons = (providers ?? []).filter((p) => p.kind === "oidc");
  // Folded behind its own button when a provider signs people in and no
  // directory needs the form; the local administrator still has it.
  const folded = buttons.length > 0 && !(providers ?? []).some((p) => p.kind === "ldap") && !showPassword;

  // Without any account, sign-in cannot work: say how to create the first one (ADR-0071).
  useEffect(() => {
    const controller = new AbortController();
    api.setupStatus(controller.signal).then(
      (status) => setSetupPending(status.pending),
      () => {},
    );
    return () => controller.abort();
  }, []);

  // Sign-in providers turned on: a button each (ADR-0145).
  useEffect(() => {
    const controller = new AbortController();
    api.loginProviders(controller.signal).then(
      (list) => setProviders(list.providers),
      // Aborted on unmount; otherwise an older server: the password form alone.
      () => !controller.signal.aborted && setProviders([]),
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
          : e instanceof ApiError && e.status === 403
            ? { kind: "noAccess" }
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
          <h1 className={styles.brand}>
            <Logo size={32} />
            MonitorFlow
          </h1>
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

  const passwordForm = (
    <form className={styles.form} onSubmit={onSubmit} aria-label={t("auth.signIn")}>
      {failure?.kind === "credentials" && (
        <Banner tone="bad">
          <strong>{t("auth.wrong.title")}</strong> {t("auth.wrong.text")}
        </Banner>
      )}
      {failure?.kind === "noAccess" && (
        <Banner tone="bad">
          <strong>{t("signIn.noAccess")}</strong> {t("signIn.noAccessText")}
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
      <Button
        className={styles.submit}
        variant={buttons.length > 0 ? "default" : "k"}
        size="large"
        type="submit"
        disabled={pending || waiting}
      >
        {pending ? t("auth.signingIn") : waiting ? t("auth.retryingIn", { seconds: String(retryIn) }) : t("auth.signIn")}
      </Button>
    </form>
  );

  return (
    <div className={styles.page}>
      <main id="main" className={styles.main}>
        <h1 className={styles.brand}>
          <Logo size={32} />
          MonitorFlow
        </h1>
        <section className={styles.box} aria-labelledby="signin-title">
          <h2 id="signin-title" className={styles.title}>
            {t("auth.signIn")}
          </h2>
          {sso && (
            <Banner tone="bad" role="alert">
              {t(`signIn.error.${sso}`)}
            </Banner>
          )}
          {buttons.map((p) => (
            // A link, not a form: the API redirects to the provider (ADR-0145).
            <a key={p.id} className={buttonClass({ variant: "k", size: "large" }, styles.submit)} href={oidcStartHref(p.id)}>
              {t("signIn.with", { name: p.name })}
            </a>
          ))}
          {buttons.length > 0 && (
            <p className={styles.or} aria-hidden="true">
              {t("signIn.or")}
            </p>
          )}
          {providers === null ? null : folded ? (
            <Button size="large" onClick={() => setShowPassword(true)}>
              {t("signIn.withPassword")}
            </Button>
          ) : (
            passwordForm
          )}
          {folded && <p className={styles.hint}>{t("signIn.passwordFor")}</p>}
        </section>
        {host && <p className={styles.foot}>{host} · MonitorFlow</p>}
      </main>
    </div>
  );
}
