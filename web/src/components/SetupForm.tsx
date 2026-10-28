"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useRef, useState } from "react";

import { SetupHint } from "@/components/SetupHint";
import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { ApiError, LOGIN_PATH, MIN_PASSWORD_LENGTH, api, describeError, isAbortError } from "@/lib/api";

import styles from "./LoginForm.module.css";

type Failure = { kind: "link" } | { kind: "mismatch" } | { kind: "other"; message: string };

/**
 * First-run setup (ADR-0057): the one-time link `/setup/#<token>` creates the
 * first administrator. Once an account exists the page sends the user to
 * sign in.
 */
export function SetupForm() {
  const router = useRouter();
  // `undefined` until read from the fragment, in the browser only.
  const [token, setToken] = useState<string | undefined>(undefined);
  const [ready, setReady] = useState(false);
  const [username, setUsername] = useState("admin");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [pending, setPending] = useState(false);
  const [failure, setFailure] = useState<Failure | null>(null);

  // Read once: the effect strips the fragment, and may run twice in development.
  const secret = useRef<string | null>(null);

  useEffect(() => {
    if (secret.current === null) {
      secret.current = window.location.hash.slice(1);
      // Keep the secret out of the history and of anything copied from the address bar.
      if (secret.current) window.history.replaceState(null, "", window.location.pathname);
    }
    setToken(secret.current);
    const controller = new AbortController();
    api.setupStatus(controller.signal).then(
      (status) => (status.pending ? setReady(true) : router.replace(LOGIN_PATH)),
      // Unknown: let the form say what is wrong when it is sent.
      (e) => isAbortError(e) || setReady(true),
    );
    return () => controller.abort();
  }, [router]);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (password !== confirm) {
      setFailure({ kind: "mismatch" });
      return;
    }
    setPending(true);
    setFailure(null);
    try {
      await api.setup(token ?? "", username, password);
      router.replace("/");
    } catch (e) {
      if (e instanceof ApiError && e.status === 409) {
        router.replace(LOGIN_PATH);
        return;
      }
      setFailure(
        e instanceof ApiError && e.status === 410
          ? { kind: "link" }
          : e instanceof ApiError && e.status === 400
            ? { kind: "other", message: `${e.message.charAt(0).toUpperCase()}${e.message.slice(1)}.` }
            : { kind: "other", message: describeError(e) },
      );
      setPending(false);
    }
  }

  const noLink = token === "" || failure?.kind === "link";

  return (
    <div className={styles.page}>
      <main id="main" className={styles.main}>
        <h1 className={styles.brand}>MonitorFlow</h1>
        {!ready ? null : noLink ? (
          <div className={styles.box}>
            <h2 className={styles.title}>Setup link needed</h2>
            <Banner tone="warn">
              {token === "" ? (
                <strong>This address has no setup link.</strong>
              ) : (
                <>
                  <strong>This setup link is no longer valid.</strong> It was already used, replaced by a newer one, or
                  expired.
                </>
              )}
            </Banner>
            <SetupHint />
          </div>
        ) : (
          <form className={styles.box} onSubmit={onSubmit} aria-labelledby="setup-title">
            <h2 id="setup-title" className={styles.title}>
              Create the administrator
            </h2>
            <p className={styles.text}>This account manages everything. You can add colleagues once you are signed in.</p>
            {failure?.kind === "mismatch" && (
              <Banner tone="bad">
                <strong>The passwords differ.</strong> Type the same password twice.
              </Banner>
            )}
            {failure?.kind === "other" && <Banner tone="bad">{failure.message}</Banner>}
            <label className={styles.label}>
              Username
              <input
                className={styles.input}
                name="username"
                autoComplete="username"
                autoCapitalize="none"
                spellCheck={false}
                required
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </label>
            <label className={styles.label}>
              Password
              <input
                className={styles.input}
                name="password"
                type="password"
                autoComplete="new-password"
                required
                minLength={MIN_PASSWORD_LENGTH}
                aria-describedby="password-hint"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
            <p id="password-hint" className={styles.hint}>
              At least {MIN_PASSWORD_LENGTH} characters.
            </p>
            <label className={styles.label}>
              Confirm password
              <input
                className={styles.input}
                name="confirm"
                type="password"
                autoComplete="new-password"
                required
                aria-invalid={failure?.kind === "mismatch" || undefined}
                value={confirm}
                onChange={(e) => setConfirm(e.target.value)}
              />
            </label>
            <Button className={styles.submit} variant="k" size="large" type="submit" disabled={pending}>
              {pending ? "Creating…" : "Create and sign in"}
            </Button>
          </form>
        )}
      </main>
    </div>
  );
}
