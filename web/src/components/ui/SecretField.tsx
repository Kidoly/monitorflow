"use client";

import { useEffect, useId, useRef, useState } from "react";

import { Button, fieldClass } from "./Button";
import { cx } from "./cx";
import styles from "./SecretField.module.css";

/** How long the button reads "Copied". */
export const COPIED_MS = 2000;

/**
 * A secret shown once: a read-only monospace input and a Copy button that
 * reads "Copied" for 2 s and announces it through an aria-live region. Put
 * the warning banner before it in the DOM. Never log or store `value`.
 */
export function SecretField({ label, value, copyLabel = "Copy" }: { label: string; value: string; copyLabel?: string }) {
  const id = useId();
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => () => clearTimeout(timer.current), []);

  async function copy() {
    clearTimeout(timer.current);
    try {
      await navigator.clipboard.writeText(value);
      setStatus("copied");
      timer.current = setTimeout(() => setStatus("idle"), COPIED_MS);
    } catch {
      setStatus("failed");
    }
  }

  return (
    <div className={styles.field}>
      <label className={styles.label} htmlFor={id}>
        {label}
      </label>
      <span className={styles.row}>
        <input
          id={id}
          className={cx(fieldClass, styles.input)}
          readOnly
          value={value}
          spellCheck={false}
          onFocus={(e) => e.currentTarget.select()}
        />
        <Button onClick={() => void copy()}>{status === "copied" ? "Copied" : copyLabel}</Button>
      </span>
      <span className="visually-hidden" aria-live="polite">
        {status === "copied" ? "Copied" : status === "failed" ? "Copying failed; select the text instead." : ""}
      </span>
    </div>
  );
}
