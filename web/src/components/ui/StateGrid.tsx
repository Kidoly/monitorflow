import Link from "next/link";

import styles from "./StateGrid.module.css";

export interface StateSquare {
  key: string;
  /** The server and its state or value, for the tooltip and screen readers. */
  name: string;
  color: string;
  opacity?: number;
  href?: string;
}

/**
 * A square per server, coloured by its state or value, worst first, with
 * a key of what the colours mean. Wraps with margins, no flex gap, so a
 * 2019 TV draws it too (ADR-0099).
 */
export function StateGrid({
  squares,
  legend,
  label,
}: {
  squares: StateSquare[];
  legend: { key: string; label: string; color: string }[];
  label: string;
}) {
  return (
    <div className={styles.box}>
      <ul className={styles.grid} aria-label={label}>
        {squares.map((s) => (
          <li key={s.key} className={styles.item}>
            {s.href ? (
              <Link href={s.href} className={styles.square} title={s.name} aria-label={s.name} style={{ background: s.color, opacity: s.opacity }} />
            ) : (
              <span className={styles.square} title={s.name} aria-label={s.name} role="img" style={{ background: s.color, opacity: s.opacity }} />
            )}
          </li>
        ))}
      </ul>
      {legend.length > 0 && (
        <ul className={styles.legend}>
          {legend.map((l) => (
            <li key={l.key}>
              <i className={styles.swatch} style={{ background: l.color }} aria-hidden="true" />
              {l.label}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
