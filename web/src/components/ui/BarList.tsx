import type { ReactNode } from "react";

import styles from "./BarList.module.css";

/** One row: its name, its value and how full its bar is. */
export interface Bar {
  key: string;
  label: ReactNode;
  value: number | null;
  /** The bar's colour, a CSS value. */
  color: string;
}

/**
 * Horizontal bars, one per row, between `min` and `max`: the "bar gauge"
 * of a dashboard. Plain boxes with margins, no flex gap, so a 2019 TV
 * draws them too (ADR-0099).
 */
export function BarList({
  bars,
  min,
  max,
  format,
  label,
}: {
  bars: Bar[];
  min: number;
  max: number;
  format: (v: number) => string;
  label: string;
}) {
  return (
    <ol className={styles.bars} aria-label={label}>
      {bars.map((bar) => {
        const share = bar.value === null ? 0 : Math.min(Math.max((bar.value - min) / (max - min || 1), 0), 1);
        return (
          <li key={bar.key} className={styles.bar}>
            <div className={styles.line}>
              <span className={styles.name}>{bar.label}</span>
              <b className={styles.value}>{bar.value === null ? "–" : format(bar.value)}</b>
            </div>
            <div className={styles.track} aria-hidden="true">
              <div className={styles.fill} style={{ width: `${(share * 100).toFixed(1)}%`, background: bar.color }} />
            </div>
          </li>
        );
      })}
    </ol>
  );
}
