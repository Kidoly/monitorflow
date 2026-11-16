"use client";

import { type ReactNode, useId } from "react";

import styles from "./Hint.module.css";
import { Label } from "./StatusLabel";
import type { Tone } from "./tone";

/**
 * A short badge for a state that is not the default ("Rollback limited ⓘ"),
 * with the explanation in a tooltip on hover or keyboard focus. The
 * explanation is also the badge's accessible description. Text only: an
 * action belongs on the page, not in a tooltip.
 */
export function Hint({ tone, label, children }: { tone: Tone; label: string; children: ReactNode }) {
  return (
    <Tooltip text={children}>
      <Label tone={tone}>
        {label}
        <span className={styles.icon} aria-hidden="true">
          ⓘ
        </span>
      </Label>
    </Tooltip>
  );
}

/** An ⓘ that explains the normal state of a page or a section, on hover or keyboard focus. */
export function InfoTip({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Tooltip text={children}>
      <span className={styles.info} role="img" aria-label={label}>
        ⓘ
      </span>
    </Tooltip>
  );
}

/** Any short text with a longer one on hover or keyboard focus ("1 min · > 85%" and its sentence). */
export function Tooltip({ text, children }: { text: ReactNode; children: ReactNode }) {
  const id = useId();
  return (
    <span className={styles.hint} tabIndex={0} aria-describedby={id}>
      {children}
      <span role="tooltip" id={id} className={styles.tip}>
        {text}
      </span>
    </span>
  );
}
