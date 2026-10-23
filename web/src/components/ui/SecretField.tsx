"use client";

import { useEffect, useId, useRef, useState } from "react";

import { Button, type ButtonSize, fieldClass } from "./Button";
import { cx } from "./cx";
import styles from "./SecretField.module.css";

/** How long the button reads "Copied". */
export const COPIED_MS = 2000;

/**
 * A Copy button that reads "Copied" for 2 s and announces it through an
 * aria-live region; `name` is its accessible name when "Copy" alone is
 * ambiguous ("Copy token"). Never log or store `text`.
 */
export function CopyButton({
  text,
  label = "Copy",
  name,
  size,
  className,
}: {
  text: string;
  label?: string;
  name?: string;
  size?: ButtonSize;
  className?: string;
}) {
  const [status, setStatus] = useState<"idle" | "copied" | "failed">("idle");
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  useEffect(() => () => clearTimeout(timer.current), []);

  async function copy() {
    clearTimeout(timer.current);
    try {
      await navigator.clipboard.writeText(text);
      setStatus("copied");
      timer.current = setTimeout(() => setStatus("idle"), COPIED_MS);
    } catch {
      setStatus("failed");
    }
  }

  return (
    <>
      <Button size={size} className={className} aria-label={name} onClick={() => void copy()}>
        {status === "copied" ? "Copied" : label}
      </Button>
      <span className="visually-hidden" aria-live="polite">
        {status === "copied" ? "Copied" : status === "failed" ? "Copying failed; select the text instead." : ""}
      </span>
    </>
  );
}

/**
 * A secret shown once: a read-only monospace input and a `CopyButton`.
 * Put the warning banner before it in the DOM. Never log or store `value`.
 */
export function SecretField({
  label,
  value,
  copyLabel = "Copy",
  copyName,
}: {
  label: string;
  value: string;
  copyLabel?: string;
  /** Accessible name of the Copy button: "Copy token". */
  copyName?: string;
}) {
  const id = useId();
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
        <CopyButton text={value} label={copyLabel} name={copyName} />
      </span>
    </div>
  );
}
