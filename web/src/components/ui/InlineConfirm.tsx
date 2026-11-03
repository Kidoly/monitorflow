"use client";

import { type FormEvent, type ReactNode, useEffect, useRef, useState } from "react";

import { Button, type ButtonVariant, buttonClass, fieldClass } from "./Button";
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
 *
 * `acknowledge` adds a checkbox the user must tick first, for actions
 * with lasting effects (an emergency change of certificate authority).
 * `confirmVariant="k"` draws the confirm button black, for a safe action.
 *
 * `layout="row"` is the compact form inside a table row (enrollment tokens):
 * one line with the question, Cancel and the danger button, all 30 px.
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
  layout = "box",
  triggerVariant = "danger",
  triggerName,
  acknowledge,
  confirmVariant = "danger",
}: {
  /** Label of the trigger button: "Revoke web-01…". */
  trigger: string;
  /** Accessible name of the trigger when its label alone is ambiguous, as in a table row. */
  triggerName?: string;
  triggerVariant?: ButtonVariant;
  layout?: "box" | "row";
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
  /** Label of a checkbox that must be ticked before confirming. */
  acknowledge?: string;
  confirmVariant?: "danger" | "k";
}) {
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState<R | "">("");
  const [acknowledged, setAcknowledged] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const selectRef = useRef<HTMLSelectElement>(null);
  const checkRef = useRef<HTMLInputElement>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  // Focus moves only after the user opened or cancelled, not on first render.
  const moved = useRef<"open" | "closed" | null>(null);

  useEffect(() => {
    if (moved.current === "open") (selectRef.current ?? checkRef.current ?? confirmRef.current)?.focus();
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
    setAcknowledged(false);
    setError(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    if ((reasons && !reason) || (acknowledge && !acknowledged)) return;
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

  const size = layout === "row" ? "small" : undefined;
  if (!open) {
    return (
      <Button
        ref={triggerRef}
        variant={triggerVariant}
        size={size}
        aria-label={triggerName}
        aria-expanded={false}
        onClick={show}
      >
        {trigger}
      </Button>
    );
  }
  if (layout === "row") {
    return (
      <form className={styles.row} onSubmit={submit} aria-label={title}>
        <span>
          <strong>{title}</strong> {consequence}
        </span>
        <Button size="small" onClick={cancel} disabled={busy}>
          Cancel
        </Button>
        <button ref={confirmRef} type="submit" className={buttonClass({ variant: "danger", size: "small" })} disabled={busy}>
          {busy ? pendingLabel : confirmLabel}
        </button>
        {error && (
          <span className={styles.error} role="alert">
            {error}
          </span>
        )}
      </form>
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
      {acknowledge && (
        <label className={styles.acknowledge}>
          <input
            ref={checkRef}
            type="checkbox"
            checked={acknowledged}
            onChange={(e) => setAcknowledged(e.target.checked)}
            disabled={busy}
          />
          <span>{acknowledge}</span>
        </label>
      )}
      <div className={styles.buttons}>
        <Button onClick={cancel} disabled={busy}>
          Cancel
        </Button>
        <button
          ref={confirmRef}
          type="submit"
          className={buttonClass({ variant: confirmVariant })}
          disabled={busy || (reasons !== undefined && !reason) || (acknowledge !== undefined && !acknowledged)}
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
