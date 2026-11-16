"use client";

import { type FormEvent, useEffect, useRef, useState } from "react";

import { Button, type ButtonVariant, buttonClass, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { t } from "@/i18n";

import styles from "./AcknowledgeButton.module.css";

/** Longest note the central server takes, in characters. */
export const MAX_NOTE = 500;

/**
 * "Acknowledge", then in place, never in a popover: a note field ("What
 * are you doing about it?"), Cancel and Acknowledge, which stays disabled
 * until the note says something (ADR-0130). Focus goes to the field on
 * open and back to the button on Cancel or Escape. `onAcknowledge`
 * rejecting keeps the form open with its error.
 */
export function AcknowledgeButton({
  label = t("incidents.button.acknowledge"),
  name,
  variant,
  size,
  disabled,
  onAcknowledge,
  describeError,
}: {
  label?: string;
  /** Accessible name of the trigger when its label alone is ambiguous, as in a table row. */
  name?: string;
  variant?: ButtonVariant;
  size?: "small";
  disabled?: boolean;
  onAcknowledge: (note: string) => Promise<void>;
  describeError: (error: unknown) => string;
}) {
  const [open, setOpen] = useState(false);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  // Focus moves only after the user opened or closed it, not on first render.
  const moved = useRef<"open" | "closed" | null>(null);

  useEffect(() => {
    if (moved.current === "closed") trigger.current?.focus();
    if (moved.current === "closed") moved.current = null;
  }, [open]);

  // The field takes focus as it appears, after the user opened it.
  const field = (input: HTMLInputElement | null) => {
    if (input && moved.current === "open") {
      moved.current = null;
      input.focus();
    }
  };

  function show() {
    moved.current = "open";
    setOpen(true);
  }

  function cancel() {
    moved.current = "closed";
    setOpen(false);
    setNote("");
    setError(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!note.trim()) return;
    setBusy(true);
    setError(null);
    try {
      await onAcknowledge(note.trim());
      setOpen(false);
      setNote("");
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <Button ref={trigger} variant={variant} size={size} aria-label={name} disabled={disabled} onClick={show}>
        {label}
      </Button>
    );
  }
  return (
    <form
      className={styles.form}
      onSubmit={submit}
      aria-label={name ?? label}
      onKeyDown={(e) => {
        if (e.key === "Escape" && !busy) cancel();
      }}
    >
      <label className={styles.label}>
        <span>{t("incidents.button.question")}</span>
        <input
          ref={field}
          className={cx(fieldClass, styles.field)}
          value={note}
          maxLength={MAX_NOTE}
          placeholder={t("incidents.button.placeholder")}
          required
          disabled={busy}
          onChange={(e) => setNote(e.target.value)}
        />
      </label>
      <Button size="small" onClick={cancel} disabled={busy}>
        {t("incidents.button.cancel")}
      </Button>
      <button type="submit" className={buttonClass({ variant: "k", size: "small" })} disabled={busy || !note.trim()}>
        {busy ? t("incidents.button.busy") : label}
      </button>
      {error && (
        <span className={styles.error} role="alert">
          {error}
        </span>
      )}
    </form>
  );
}
