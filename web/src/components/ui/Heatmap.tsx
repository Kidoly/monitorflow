import type { ReactNode } from "react";

import { buckets } from "./chartScale";
import styles from "./Heatmap.module.css";

/** Most cells across a row; more would be slivers. */
export const MAX_CELLS = 48;

export interface HeatRow {
  key: string;
  label: ReactNode;
  /** What a screen reader and a tooltip name the row. */
  name: string;
  points: [number, number | null][];
}

/**
 * A row per series over time, each period a cell coloured by its value:
 * `color(value)` gives the cell's colour and strength. Plain boxes, no
 * flex gap, so a 2019 TV draws them too (ADR-0099).
 */
export function Heatmap({
  rows,
  start,
  end,
  step,
  format,
  color,
  label,
}: {
  rows: HeatRow[];
  start: number;
  end: number;
  step: number;
  format: (v: number) => string;
  /** A cell's background and opacity, from its value. */
  color: (value: number) => { background: string; opacity: number };
  label: string;
}) {
  return (
    <div className={styles.heatmap} role="table" aria-label={label}>
      {rows.map((row) => {
        const cells = buckets(row.points, start, end, step, MAX_CELLS).points;
        const worst = cells.reduce<number | null>((w, [, v]) => (v === null ? w : w === null ? v : Math.max(w, v)), null);
        return (
          <div key={row.key} className={styles.row} role="row">
            <span className={styles.name} role="rowheader">
              {row.label}
            </span>
            <span className={styles.cells} role="cell" aria-label={worst === null ? row.name : `${row.name}: ${format(worst)}`}>
              <span className={styles.strip}>
                {cells.map(([t, v]) => {
                  const c = v === null ? null : color(v);
                  return (
                    <i
                      key={t}
                      className={styles.cell}
                      title={v === null ? undefined : `${new Date(t * 1000).toLocaleString()}: ${format(v)}`}
                      style={c ? { background: c.background, opacity: c.opacity } : undefined}
                    />
                  );
                })}
              </span>
            </span>
          </div>
        );
      })}
    </div>
  );
}
