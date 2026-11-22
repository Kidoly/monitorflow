"use client";

import { type KeyboardEvent, type ReactNode, useRef } from "react";

import styles from "./SegmentedControl.module.css";
import { cx } from "./cx";

export interface Segment<T extends string> {
  value: T;
  label: ReactNode;
  disabled?: boolean;
}

interface SegmentedControlProps<T extends string> {
  options: readonly Segment<T>[];
  value: T;
  onChange: (value: T) => void;
  /** Names the group when no visible legend does (`aria-labelledby` otherwise). */
  label?: string;
  labelledBy?: string;
  disabled?: boolean;
  className?: string;
}

/**
 * One choice among two to five options, drawn as grouped buttons with the
 * chosen one filled with the accent. A radio group for assistive technology:
 * Tab reaches the chosen option, the arrow keys move and choose, Home and End
 * jump to the ends.
 */
export function SegmentedControl<T extends string>({
  options,
  value,
  onChange,
  label,
  labelledBy,
  disabled,
  className,
}: SegmentedControlProps<T>) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const enabled = options.map((o, i) => (disabled || o.disabled ? -1 : i)).filter((i) => i >= 0);
  const current = options.findIndex((o) => o.value === value);
  // The one tab stop: the chosen option, or the first one that can be chosen.
  const tabStop = enabled.includes(current) ? current : (enabled[0] ?? -1);

  const move = (from: number, step: number | "first" | "last") => {
    let next: number | undefined;
    if (step === "first") next = enabled[0];
    else if (step === "last") next = enabled[enabled.length - 1];
    else {
      const at = Math.max(0, enabled.indexOf(from));
      next = enabled[(at + step + enabled.length) % enabled.length];
    }
    const option = next === undefined ? undefined : options[next];
    if (next === undefined || !option) return;
    refs.current[next]?.focus();
    if (option.value !== value) onChange(option.value);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>, i: number) => {
    const keys: Record<string, number | "first" | "last"> = {
      ArrowRight: 1,
      ArrowDown: 1,
      ArrowLeft: -1,
      ArrowUp: -1,
      Home: "first",
      End: "last",
    };
    const step = keys[e.key];
    if (step === undefined) return;
    e.preventDefault();
    move(i, step);
  };

  return (
    <div
      role="radiogroup"
      aria-label={labelledBy ? undefined : label}
      aria-labelledby={labelledBy}
      aria-disabled={disabled || undefined}
      className={cx(styles.group, className)}
    >
      {options.map((o, i) => (
        <button
          key={o.value}
          ref={(el) => {
            refs.current[i] = el;
          }}
          type="button"
          role="radio"
          aria-checked={o.value === value}
          tabIndex={i === tabStop ? 0 : -1}
          disabled={disabled || o.disabled}
          className={styles.option}
          onClick={() => o.value !== value && onChange(o.value)}
          onKeyDown={(e) => onKeyDown(e, i)}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
