"use client";

import type { ComponentProps, ReactNode } from "react";

import styles from "./Switch.module.css";
import { cx } from "./cx";

type ButtonProps = Omit<ComponentProps<"button">, "onChange" | "role" | "type" | "children" | "value">;

interface SwitchProps extends ButtonProps {
  checked: boolean;
  onChange: (checked: boolean) => void;
  /** The words next to the switch; they also name it. */
  label?: ReactNode;
  /** A muted line after the label ("Only for a trusted receiver"). */
  note?: ReactNode;
  /** Class of the row around the switch and its label. */
  rowClassName?: string;
}

/**
 * A yes/no choice: the knob slides right and the track fills with the accent
 * when it is on. A `<button role="switch">` inside a `<label>`, so a click on
 * the words toggles it, Space and Enter toggle it from the keyboard, and the
 * label is its accessible name. Real checkboxes stay for picking items in a
 * list; a choice between a few options is a `SegmentedControl`.
 */
export function Switch({ checked, onChange, label, note, rowClassName, className, disabled, onClick, ...props }: SwitchProps) {
  const button = (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      className={cx(styles.switch, className)}
      onClick={(e) => {
        onClick?.(e);
        if (!e.defaultPrevented) onChange(!checked);
      }}
      {...props}
    >
      <span className={styles.knob} aria-hidden="true" />
    </button>
  );
  if (label === undefined && note === undefined) return button;
  return (
    <label className={cx(styles.row, disabled && styles.disabled, rowClassName)}>
      {button}
      <span className={styles.text}>
        {label}
        {note !== undefined && <span className={styles.note}> {note}</span>}
      </span>
    </label>
  );
}
