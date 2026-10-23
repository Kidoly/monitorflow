"use client";

import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";

import { Button, buttonClass, fieldClass } from "./Button";
import { cx } from "./cx";
import styles from "./InlineConfirm.module.css";

/**
 * A dangerous action confirmed in place, never in a modal: the trigger
 * button is replaced by a bordered box with the consequence, a reason
 * `<select>`, Cancel and a danger button naming the target ("Revoke
 * web-01"), disabled until a reason is chosen. Focus goes to the select on
 * open (the confirm button when there are no reasons) and back to the
 * trigger on cancel. `onConfirm` rejecting keeps the box open with
 * `describeError`'s text.
 */
export function InlineConfirm<R extends string>({
  trigger,
  title,
  consequence,
  reasons,
  confirmLabel,
  pendingLabel = "Working…",
  onConfirm,
  describeError = () => "It failed. Try again.",
}: {
  /** Label of the trigger button: "Revoke web-01…". */
  trigger: string;
  /** Bold question opening the box: "Revoke web-01?". */
  title: string;
  consequence: ReactNode;
  /** Choices of the reason select; omit when the action takes none. */
  reasons?: { value: R; label: string }[];
  /** Danger button; names the target. */
  confirmLabel: string;
  pendingLabel?: string;
  onConfirm: (reason: R | null) => Promise<void> | void;
  describeError?: (error: unknown) => string;
}) {
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState<R | "">("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const selectRef = useRef<HTMLSelectElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  // Focus moves only after the user opened or cancelled, not on first render.
  const moved = useRef<"open" | "closed" | null>(null);

  useEffect(() => {
    if (moved.current === "open") (selectRef.current ?? confirmRef.current)?.focus();
    if (moved.current === "closed") triggerRef.current?.focus();
    moved.current = null;
  }, [open]);

  function show() {
    moved.current = "open";
    setOpen(true);
  }

  function cancel() {
    moved.current = "closed";
    setOpen(false);
    setReason("");
    setError(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (reasons && !reason) return;
    setBusy(true);
    setError(null);
    try {
      await onConfirm(reason || null);
      setOpen(false);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <Button ref={triggerRef} variant="danger" aria-expanded={false} onClick={show}>
        {trigger}
      </Button>
    );
  }
  return (
    <form className={styles.box} onSubmit={submit} aria-label={title}>
      <p className={styles.consequence}>
        <strong>{title}</strong> {consequence}
      </p>
      {reasons && (
        <label className={styles.reason}>
          <span>Reason</span>
          <select
            ref={selectRef}
            className={cx(fieldClass, styles.select)}
            value={reason}
            onChange={(e) => setReason(e.target.value as R)}
            disabled={busy}
          >
            <option value="">Choose a reason…</option>
            {reasons.map((r) => (
              <option key={r.value} value={r.value}>
                {r.label}
              </option>
            ))}
          </select>
        </label>
      )}
      <div className={styles.buttons}>
        <Button onClick={cancel} disabled={busy}>
          Cancel
        </Button>
        <button
          ref={confirmRef}
          type="submit"
          className={buttonClass({ variant: "danger" })}
          disabled={busy || (reasons !== undefined && !reason)}
        >
          {busy ? pendingLabel : confirmLabel}
        </button>
      </div>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}
