import styles from "./Gauge.module.css";

/** Where the dial starts and how far it turns, in degrees, clockwise from three o'clock. */
const FROM = 150;
const SWEEP = 240;
const CX = 100;
const CY = 96;
const R = 78;

const point = (angle: number, r = R) => {
  const rad = (angle * Math.PI) / 180;
  return `${(CX + r * Math.cos(rad)).toFixed(2)} ${(CY + r * Math.sin(rad)).toFixed(2)}`;
};

/** The SVG path of the dial from `a` to `b`, fractions of the sweep. */
export function arc(a: number, b: number, r = R): string {
  const from = FROM + SWEEP * Math.min(Math.max(a, 0), 1);
  const to = FROM + SWEEP * Math.min(Math.max(b, 0), 1);
  return `M ${point(from, r)} A ${r} ${r} 0 ${to - from > 180 ? 1 : 0} 1 ${point(to, r)}`;
}

/** `value`'s place between `min` and `max`, from 0 to 1. */
export const fraction = (value: number, min: number, max: number): number =>
  Math.min(Math.max((value - min) / (max - min || 1), 0), 1);

/**
 * One value on a dial between `min` and `max`, drawn in SVG so it scales
 * with its box, on a page or a screen. The thresholds colour a thin outer
 * ring; the value's arc takes the colour of the band it is in.
 */
export function Gauge({
  value,
  min,
  max,
  format,
  color,
  bands = [],
  label,
}: {
  value: number | null;
  min: number;
  max: number;
  format: (v: number) => string;
  /** The value arc's colour, a CSS value. */
  color: string;
  /** Coloured stretches of the outer ring, in values. */
  bands?: { from: number; to: number; color: string }[];
  /** What a screen reader hears. */
  label: string;
}) {
  return (
    <svg className={styles.gauge} viewBox="0 0 200 164" role="img" aria-label={label}>
      <path className={styles.track} d={arc(0, 1)} />
      {bands.map((b) => (
        <path
          key={`${b.from}-${b.to}`}
          className={styles.band}
          d={arc(fraction(b.from, min, max), fraction(b.to, min, max), R + 11)}
          stroke={b.color}
        />
      ))}
      {value !== null && fraction(value, min, max) > 0 && (
        <path className={styles.value} d={arc(0, fraction(value, min, max))} stroke={color} data-testid="gauge-value" />
      )}
      <text className={styles.figure} x={CX} y={CY + 8} textAnchor="middle">
        {value === null ? "–" : format(value)}
      </text>
      {/* Under the ends of the dial, clear of its round caps. */}
      <text className={styles.end} x={CX - 68} y={160} textAnchor="middle">
        {format(min)}
      </text>
      <text className={styles.end} x={CX + 68} y={160} textAnchor="middle">
        {format(max)}
      </text>
    </svg>
  );
}
