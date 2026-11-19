"use client";

import { t } from "@/i18n";
import { MIN_PASSWORD_LENGTH } from "@/lib/api";

import styles from "./LoginForm.module.css";

/** A new password and its confirmation, for the setup and welcome links. */
export function NewPasswordFields({
  password,
  confirm,
  mismatch,
  onPassword,
  onConfirm,
}: {
  password: string;
  confirm: string;
  /** The two did not match when the form was sent. */
  mismatch: boolean;
  onPassword: (value: string) => void;
  onConfirm: (value: string) => void;
}) {
  return (
    <>
      <label className={styles.label}>
        {t("auth.password")}
        <input
          className={styles.input}
          name="password"
          type="password"
          autoComplete="new-password"
          required
          minLength={MIN_PASSWORD_LENGTH}
          aria-describedby="password-hint"
          value={password}
          onChange={(e) => onPassword(e.target.value)}
        />
      </label>
      <p id="password-hint" className={styles.hint}>
        {t("auth.passwordHint", { count: MIN_PASSWORD_LENGTH })}
      </p>
      <label className={styles.label}>
        {t("auth.confirmPassword")}
        <input
          className={styles.input}
          name="confirm"
          type="password"
          autoComplete="new-password"
          required
          aria-invalid={mismatch || undefined}
          value={confirm}
          onChange={(e) => onConfirm(e.target.value)}
        />
      </label>
    </>
  );
}
