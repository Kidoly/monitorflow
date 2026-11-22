import styles from "./Donut.module.css";

export interface DonutSlice {
  key: string;
  label: string;
  value: number;
  color: string;
}

const R = 46;
const TURN = 2 * Math.PI * R;

/**
 * A ring of slices, the largest share of a whole, with the total in its
 * middle and a key naming each slice and its share. Drawn with dashed
 * circles, which every browser of ADR-0099 draws.
 */
export function Donut({
  slices,
  total,
  format,
  label,
}: {
  slices: DonutSlice[];
  /** The text in the middle. */
  total: string;
  format: (v: number) => string;
  label: string;
}) {
  const sum = slices.reduce((a, s) => a + s.value, 0) || 1;
  // Where each slice starts along the ring.
  const starts = slices.map((_, i) => slices.slice(0, i).reduce((a, s) => a + (s.value / sum) * TURN, 0));
  return (
    <div className={styles.box}>
      <svg className={styles.ring} viewBox="0 0 120 120" role="img" aria-label={label}>
        <circle className={styles.track} cx={60} cy={60} r={R} />
        {slices.map((s, i) => (
          <circle
            key={s.key}
            className={styles.slice}
            cx={60}
            cy={60}
            r={R}
            stroke={s.color}
            strokeDasharray={`${Math.max((s.value / sum) * TURN - (slices.length > 1 ? 1 : 0), 0)} ${TURN}`}
            strokeDashoffset={-starts[i]!}
            transform="rotate(-90 60 60)"
          />
        ))}
        <text className={styles.total} x={60} y={66} textAnchor="middle">
          {total}
        </text>
      </svg>
      <ul className={styles.legend}>
        {slices.map((s) => (
          <li key={s.key}>
            <i className={styles.swatch} style={{ background: s.color }} aria-hidden="true" />
            <span className={styles.name}>{s.label}</span>
            <b>{format(s.value)}</b>
            <span className={styles.share}>{Math.round((s.value / sum) * 100)}%</span>
          </li>
        ))}
      </ul>
    </div>
  );
}
