"use client";

import { useRouter } from "next/navigation";
import { type FormEvent, useEffect, useRef, useState } from "react";

import { SetupHint } from "@/components/SetupHint";
import { Button } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { Logo } from "@/components/ui/Logo";
import { t } from "@/i18n";
import { ApiError, LOGIN_PATH, api, describeError, isAbortError } from "@/lib/api";

import { takeFragmentSecret } from "./fragmentSecret";
import { NewPasswordFields } from "./NewPasswordFields";
import styles from "./LoginForm.module.css";

type Failure = { kind: "link" } | { kind: "mismatch" } | { kind: "other"; message: string };

/**
 * First-run setup (ADR-0071): the one-time link `/setup/#<token>` creates the
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

  const secret = useRef<string | null>(null);

  useEffect(() => {
    setToken(takeFragmentSecret(secret));
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
        <h1 className={styles.brand}>
          <Logo size={32} />
          MonitorFlow
        </h1>
        {!ready ? null : noLink ? (
          <div className={styles.box}>
            <h2 className={styles.title}>{t("auth.setup.linkNeeded")}</h2>
            <Banner tone="warn">
              {token === "" ? (
                <strong>{t("auth.setup.noLink")}</strong>
              ) : (
                <>
                  <strong>{t("auth.setup.linkInvalid")}</strong> {t("auth.setup.linkInvalidText")}
                </>
              )}
            </Banner>
            <SetupHint />
          </div>
        ) : (
          <form className={styles.box} onSubmit={onSubmit} aria-labelledby="setup-title">
            <h2 id="setup-title" className={styles.title}>
              {t("auth.setup.title")}
            </h2>
            <p className={styles.text}>{t("auth.setup.text")}</p>
            {failure?.kind === "mismatch" && (
              <Banner tone="bad">
                <strong>{t("auth.mismatch.title")}</strong> {t("auth.mismatch.text")}
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
                value={username}
                onChange={(e) => setUsername(e.target.value)}
              />
            </label>
            <NewPasswordFields
              password={password}
              confirm={confirm}
              mismatch={failure?.kind === "mismatch"}
              onPassword={setPassword}
              onConfirm={setConfirm}
            />
            <Button className={styles.submit} variant="k" size="large" type="submit" disabled={pending}>
              {pending ? t("auth.setup.creating") : t("auth.setup.create")}
            </Button>
          </form>
        )}
      </main>
    </div>
  );
}
