"use client";

import { useState } from "react";

import { describeError } from "@/lib/api";
import { INTERVAL_CHOICES, INTERVAL_UNITS, customInterval, every, splitInterval } from "@/lib/intervals";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";

import styles from "./IntervalSelect.module.css";

/** Going back to an inherited interval: "every 1 min (site default)". */
export interface Inherit {
  ms: number;
  label: string;
}

const OTHER = "other";
const INHERIT = "inherit";

/**
 * How often a check runs (ADR-0097): the usual choices, the current value
 * if it is another one, "Other…" for any value from 10 s to 24 h, and
 * optionally going back to an inherited value (`null` to `onChange`).
 */
export function IntervalSelect({
  value,
  onChange,
  label,
  inherit,
  disabled,
}: {
  /** Null: the inherited value is in use. */
  value: number | null;
  onChange: (ms: number | null) => void;
  /** The accessible name, like "How often to check CPU usage". */
  label: string;
  inherit?: Inherit;
  disabled?: boolean;
}) {
  const [custom, setCustom] = useState<{ amount: string; unitMs: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const choices = value === null || INTERVAL_CHOICES.includes(value) ? INTERVAL_CHOICES : [...INTERVAL_CHOICES, value].sort((a, b) => a - b);
  const selected = custom ? OTHER : value === null ? INHERIT : String(value);

  function choose(next: string) {
    setError(null);
    if (next === OTHER) {
      setCustom(splitInterval(value ?? inherit?.ms ?? 60_000));
    } else {
      setCustom(null);
      onChange(next === INHERIT ? null : Number(next));
    }
  }

  function apply() {
    if (!custom) return;
    const result = customInterval(custom.amount, custom.unitMs);
    if ("error" in result) {
      setError(result.error);
      return;
    }
    setCustom(null);
    setError(null);
    onChange(result.ms);
  }

  return (
    <span className={styles.picker}>
      <select aria-label={label} className={cx(fieldClass, styles.select)} value={selected} disabled={disabled} onChange={(e) => choose(e.target.value)}>
        {inherit && <option value={INHERIT}>{`${every(inherit.ms)} (${inherit.label})`}</option>}
        {choices.map((ms) => (
          <option key={ms} value={String(ms)}>
            {every(ms)}
          </option>
        ))}
        <option value={OTHER}>Other…</option>
      </select>
      {custom && (
        <>
          <input
            aria-label="Every"
            className={cx(fieldClass, styles.amount)}
            inputMode="decimal"
            value={custom.amount}
            disabled={disabled}
            aria-invalid={error ? true : undefined}
            onChange={(e) => setCustom({ ...custom, amount: e.target.value })}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                e.preventDefault();
                apply();
              }
            }}
          />
          <select
            aria-label="Unit"
            className={fieldClass}
            value={custom.unitMs}
            disabled={disabled}
            onChange={(e) => setCustom({ ...custom, unitMs: Number(e.target.value) })}
          >
            {INTERVAL_UNITS.map((u) => (
              <option key={u.ms} value={u.ms}>
                {u.label}
              </option>
            ))}
          </select>
          <Button size="small" onClick={apply} disabled={disabled}>
            Set
          </Button>
          <Button
            size="small"
            onClick={() => {
              setCustom(null);
              setError(null);
            }}
            disabled={disabled}
          >
            Cancel
          </Button>
        </>
      )}
      {error && (
        <span role="alert" className={styles.error}>
          {error}
        </span>
      )}
    </span>
  );
}

/**
 * An [`IntervalSelect`] that saves each change with `save`, saying so
 * while it runs and why when it fails.
 */
export function IntervalControl({
  value,
  save,
  label,
  inherit,
}: {
  value: number | null;
  save: (ms: number | null) => Promise<void>;
  label: string;
  inherit?: Inherit;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function change(ms: number | null) {
    setBusy(true);
    setError(null);
    try {
      await save(ms);
    } catch (e) {
      setError(`It was not changed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }
  return (
    <span className={styles.picker}>
      <IntervalSelect value={value} onChange={(ms) => void change(ms)} label={label} inherit={inherit} disabled={busy} />
      {busy && <span className={styles.dim}>Saving…</span>}
      {error && (
        <span role="alert" className={styles.error}>
          {error}
        </span>
      )}
    </span>
  );
}
