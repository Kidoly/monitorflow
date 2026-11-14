import styles from "./Sparkline.module.css";
import type { Tone } from "./tone";

const W = 96;
const H = 24;

/**
 * A small line of recent values, for a table cell: no axes, gaps where a
 * point is missing. Ratios keep their 0 to 100% scale so rows compare;
 * other values fill the height. The line takes the colour of `tone`; the
 * status itself is always said in words next to it.
 */
export function Sparkline({
  points,
  start,
  end,
  ratio,
  tone = "neutral",
  label,
}: {
  points: [number, number | null][];
  start: number;
  end: number;
  ratio?: boolean;
  tone?: Tone;
  /** The accessible name, like "CPU usage, last hour: 12% to 38%". */
  label: string;
}) {
  const values = points.map((p) => p[1]).filter((v): v is number => v !== null);
  if (values.length < 2 || end <= start) return <span className={styles.none}>–</span>;
  const low = ratio ? 0 : Math.min(...values);
  const high = ratio ? Math.max(1, ...values) : Math.max(...values);
  const span = high - low || 1;
  const x = (t: number) => ((t - start) / (end - start)) * W;
  // A flat series draws in the middle rather than on the bottom edge.
  const y = (v: number) => (high === low ? H / 2 : H - 1 - ((v - low) / span) * (H - 2));
  let d = "";
  let pen = false;
  for (const [t, v] of points) {
    if (v === null) {
      pen = false;
      continue;
    }
    d += `${pen ? "L" : "M"}${x(t).toFixed(1)} ${y(v).toFixed(1)}`;
    pen = true;
  }
  return (
    <svg className={`${styles.spark} ${styles[tone]}`} width={W} height={H} viewBox={`0 0 ${W} ${H}`} role="img" aria-label={label}>
      <path d={d} fill="none" strokeWidth="1.5" strokeLinejoin="round" strokeLinecap="round" />
    </svg>
  );
}
