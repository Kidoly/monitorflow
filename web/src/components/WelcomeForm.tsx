"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useRef, useState } from "react";

import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import {
  ApiError,
  LOGIN_PATH,
  type LinkOwner,
  MIN_PASSWORD_LENGTH,
  api,
  describeError,
} from "@/lib/api";

import styles from "./LoginForm.module.css";

type Failure = { kind: "mismatch" } | { kind: "other"; message: string };

/**
 * `/welcome/#<token>` (ADR-0059): the link an administrator gives to invite
 * a user or reset their password. The user chooses a password and is
 * signed in.
 */
export function WelcomeForm() {
  const router = useRouter();
  // `undefined` while checking the link, `null` when it is missing or dead.
  const [owner, setOwner] = useState<LinkOwner | null | undefined>(undefined);
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
    if (!secret.current) {
      setOwner(null);
      return;
    }
    let current = true;
    api.inspectPasswordLink(secret.current).then(
      (found) => current && setOwner(found),
      (e) => {
        if (!current) return;
        if (e instanceof ApiError && e.status === 410) setOwner(null);
        // Unknown: the form says what is wrong when it is sent.
        else setFailure({ kind: "other", message: describeError(e) });
      },
    );
    return () => {
      current = false;
    };
  }, []);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (password !== confirm) {
      setFailure({ kind: "mismatch" });
      return;
    }
    setPending(true);
    setFailure(null);
    try {
      await api.usePasswordLink(secret.current ?? "", password);
      router.replace("/");
    } catch (e) {
      if (e instanceof ApiError && e.status === 410) {
        setOwner(null);
      } else {
        setFailure({
          kind: "other",
          message:
            e instanceof ApiError && e.status === 400
              ? `${e.message.charAt(0).toUpperCase()}${e.message.slice(1)}.`
              : describeError(e),
        });
      }
      setPending(false);
    }
  }

  return (
    <div className={styles.page}>
      <main id="main" className={styles.main}>
        <h1 className={styles.brand}>MonitorFlow</h1>
        {owner === null ? (
          <div className={styles.box}>
            <h2 className={styles.title}>Link no longer valid</h2>
            <Banner tone="warn">
              <strong>This link cannot be used.</strong> It was already used, replaced by a newer one, or expired.
            </Banner>
            <p className={styles.text}>
              Ask your administrator for a new link. If you already chose a password, <Link href={LOGIN_PATH}>sign in</Link>
              .
            </p>
          </div>
        ) : owner === undefined && !failure ? null : (
          <form className={styles.box} onSubmit={onSubmit} aria-labelledby="welcome-title">
            <h2 id="welcome-title" className={styles.title}>
              {owner?.invited === false ? "Choose a new password" : "Welcome to MonitorFlow"}
            </h2>
            {owner && (
              <p className={styles.text}>
                {owner.invited ? "Choose a password for " : "Your account is "}
                <strong>{owner.username}</strong>. You will be signed in right after.
              </p>
            )}
            {failure?.kind === "mismatch" && (
              <Banner tone="bad">
                <strong>The passwords differ.</strong> Type the same password twice.
              </Banner>
            )}
            {failure?.kind === "other" && <Banner tone="bad">{failure.message}</Banner>}
            {/* Lets password managers save the username with the password. */}
            <input type="hidden" name="username" autoComplete="username" value={owner?.username ?? ""} />
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
              {pending ? "Saving…" : "Save and sign in"}
            </Button>
          </form>
        )}
      </main>
    </div>
  );
}
