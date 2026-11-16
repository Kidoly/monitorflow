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
  const id = useId();
  return (
    <span className={styles.hint} tabIndex={0} aria-describedby={id}>
      <Label tone={tone}>
        {label}
        <span className={styles.icon} aria-hidden="true">
          ⓘ
        </span>
      </Label>
      <span role="tooltip" id={id} className={styles.tip}>
        {children}
      </span>
    </span>
  );
}
