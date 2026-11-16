"use client";

import { type FormEvent, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { t } from "@/i18n";
import { ApiError, MIN_PASSWORD_LENGTH, api, describeError } from "@/lib/api";

import styles from "./Settings.module.css";

type Outcome =
  { kind: "done" } | { kind: "mismatch" } | { kind: "failed"; message: string };

/**
 * The signed-in person changes their password (ADR-0141): the current one,
 * then the new one twice. The server ends their other sessions and keeps
 * this one, so they stay signed in here.
 */
export function PasswordForm({ username }: { username: string }) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [pending, setPending] = useState(false);
  const [outcome, setOutcome] = useState<Outcome | null>(null);

  async function onSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (next !== confirm) {
      setOutcome({ kind: "mismatch" });
      return;
    }
    setPending(true);
    setOutcome(null);
    try {
      await api.changePassword(current, next);
      setCurrent("");
      setNext("");
      setConfirm("");
      setOutcome({ kind: "done" });
    } catch (e) {
      setOutcome({
        kind: "failed",
        // The server's 400 sentences are plain and safe to show.
        message:
          e instanceof ApiError && e.status === 400
            ? `${e.message.charAt(0).toUpperCase()}${e.message.slice(1).replace(/\.?$/, ".")}`
            : describeError(e),
      });
    } finally {
      setPending(false);
    }
  }

  return (
    <form
      className={styles.password}
      onSubmit={onSubmit}
      aria-labelledby="preferences-password"
    >
      {outcome?.kind === "done" && (
        <Banner tone="ok" role="status">
          {t("settings.preferences.password.done")}
        </Banner>
      )}
      {outcome?.kind === "mismatch" && (
        <Banner tone="bad">
          <strong>{t("auth.mismatch.title")}</strong> {t("auth.mismatch.text")}
        </Banner>
      )}
      {outcome?.kind === "failed" && (
        <Banner tone="bad">{outcome.message}</Banner>
      )}
      {/* Lets password managers update the right entry. */}
      <input
        type="hidden"
        name="username"
        autoComplete="username"
        value={username}
      />
      <label className={styles.field}>
        {t("settings.preferences.password.current")}
        <input
          className={fieldClass}
          type="password"
          name="current-password"
          autoComplete="current-password"
          required
          value={current}
          onChange={(e) => setCurrent(e.target.value)}
        />
      </label>
      <label className={styles.field}>
        {t("settings.preferences.password.new")}
        <input
          className={fieldClass}
          type="password"
          name="new-password"
          autoComplete="new-password"
          required
          minLength={MIN_PASSWORD_LENGTH}
          aria-describedby="new-password-hint"
          value={next}
          onChange={(e) => setNext(e.target.value)}
        />
      </label>
      <p id="new-password-hint" className={styles.fieldHint}>
        {t("auth.passwordHint", { count: MIN_PASSWORD_LENGTH })}{" "}
        {t("settings.preferences.password.others")}
      </p>
      <label className={styles.field}>
        {t("auth.confirmPassword")}
        <input
          className={fieldClass}
          type="password"
          name="confirm-password"
          autoComplete="new-password"
          required
          aria-invalid={outcome?.kind === "mismatch" || undefined}
          value={confirm}
          onChange={(e) => setConfirm(e.target.value)}
        />
      </label>
      <div>
        <Button type="submit" disabled={pending}>
          {pending
            ? t("settings.preferences.password.saving")
            : t("settings.preferences.password.save")}
        </Button>
      </div>
    </form>
  );
}
