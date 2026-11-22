import type { ReactNode } from "react";

import styles from "./Timeline.module.css";

export interface TimelineSpan {
  start: number;
  end: number;
  severity: "warning" | "critical";
}

export interface TimelineRow {
  key: string;
  label: ReactNode;
  spans: TimelineSpan[];
}

/** Seconds of `[start, end]` covered by `spans`, overlaps counted once. */
export function covered(spans: TimelineSpan[], start: number, end: number): number {
  let total = 0;
  let reach = start;
  for (const s of [...spans].sort((a, b) => a.start - b.start)) {
    const from = Math.max(s.start, reach);
    const to = Math.min(s.end, end);
    if (to > from) total += to - from;
    reach = Math.max(reach, s.end);
  }
  return total;
}

/**
 * A row per server over `[start, end]`: amber and red stretches where it
 * had open alerts, red drawn over amber, and the share of the time it had
 * none on the right. Boxes positioned in percent, so a 2019 TV draws it
 * too (ADR-0099).
 */
export function Timeline({
  rows,
  start,
  end,
  share,
  ticks,
  label,
}: {
  rows: TimelineRow[];
  start: number;
  end: number;
  /** "99.2% without alerts", from the share of clear time. */
  share: (clear: number) => string;
  /** Labels under the rows, at the start, middle and end. */
  ticks: [string, string, string];
  label: string;
}) {
  const width = Math.max(end - start, 1);
  const place = (s: TimelineSpan) => ({
    left: `${(((Math.max(s.start, start) - start) / width) * 100).toFixed(2)}%`,
    width: `${Math.max(((Math.min(s.end, end) - Math.max(s.start, start)) / width) * 100, 0.3).toFixed(2)}%`,
  });
  return (
    <div className={styles.timeline} role="table" aria-label={label}>
      {rows.map((row) => {
        const clear = 1 - covered(row.spans, start, end) / width;
        const ordered = [...row.spans].sort((a, b) => (a.severity === b.severity ? 0 : a.severity === "warning" ? -1 : 1));
        return (
          <div key={row.key} className={styles.row} role="row">
            <span className={styles.name} role="rowheader">
              {row.label}
            </span>
            <span className={styles.track} role="cell">
              {ordered.map((s) => (
                <i key={`${s.start}-${s.end}-${s.severity}`} className={styles[s.severity]} style={place(s)} />
              ))}
            </span>
            <span className={styles.share} role="cell">
              {share(clear)}
            </span>
          </div>
        );
      })}
      <div className={styles.row} aria-hidden="true">
        <span className={styles.name} />
        <span className={styles.ticks}>
          <span>{ticks[0]}</span>
          <span>{ticks[1]}</span>
          <span>{ticks[2]}</span>
        </span>
        <span className={styles.share} />
      </div>
    </div>
  );
}
