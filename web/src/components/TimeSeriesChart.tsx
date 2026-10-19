"use client";

import { type PointerEvent, useId, useState } from "react";

import { type Unit, binaryExponent, formatBytes, formatValue } from "@/lib/format";
import type { Severity } from "@/lib/thresholds";

import styles from "./TimeSeriesChart.module.css";

/** One line: `[unix seconds, value]` pairs, sorted by time; null is a gap. */
export interface ChartSeries {
  label: string;
  points: [number, number | null][];
}

/**
 * A horizontal line at `value`, for a threshold. The chart only draws it;
 * where the values come from is the caller's business.
 */
export interface ReferenceLine {
  value: number;
  label: string;
  severity: Severity;
}

/** A vertical line at time `t` (unix seconds), such as when an incident opened. */
export interface TimeMarker {
  t: number;
  label: string;
}

/** Series colours in a fixed order (validated light palette); never cycled. */
export const MAX_SERIES = 8;

const WIDTH = 640;
const HEIGHT = 220;
const MARGIN = { top: 12, right: 12, bottom: 26, left: 64 };
const PLOT_W = WIDTH - MARGIN.left - MARGIN.right;
const PLOT_H = HEIGHT - MARGIN.top - MARGIN.bottom;

interface Props {
  title: string;
  unit: Unit;
  /** Visible range, in unix seconds. */
  start: number;
  end: number;
  series: ChartSeries[];
  /** Expected seconds between points; a longer gap breaks the line. */
  step?: number;
  referenceLines?: ReferenceLine[];
  markers?: TimeMarker[];
  /** Shown instead of the plot when no series has a value. */
  emptyText?: string;
}

/** A line chart drawn as SVG, one line per series, with a hover readout. */
export function TimeSeriesChart({
  title,
  unit,
  start,
  end,
  series: allSeries,
  step,
  referenceLines = [],
  markers = [],
  emptyText = "No data in this range.",
}: Props) {
  const titleId = useId();
  const [hover, setHover] = useState<number | null>(null);
  const series = allSeries.slice(0, MAX_SERIES);
  const hidden = allSeries.length - series.length;
  const values = series.flatMap((s) => s.points.map((p) => p[1])).filter((v) => v !== null);

  const [low, high] = yDomain(unit, values, referenceLines);
  const x = (t: number) => MARGIN.left + ((t - start) / Math.max(end - start, 1)) * PLOT_W;
  const y = (v: number) => MARGIN.top + PLOT_H - ((v - low) / (high - low)) * PLOT_H;
  const times = [...new Set(series.flatMap((s) => s.points.map((p) => p[0])))].sort((a, b) => a - b);

  function onPointerMove(event: PointerEvent<SVGSVGElement>) {
    const rect = event.currentTarget.getBoundingClientRect();
    if (rect.width === 0 || times.length === 0) return;
    const t = start + (((event.clientX - rect.left) / rect.width) * WIDTH - MARGIN.left) / PLOT_W * (end - start);
    setHover(nearest(times, t));
  }

  // A series named like the chart (one unnamed line) needs no name of its own.
  const nameOf = (s: ChartSeries) => (s.label === title ? "" : ` ${s.label}`);
  const readoutAt = (s: ChartSeries) =>
    hover === null
      ? [...s.points].reverse().find((p) => p[1] !== null)?.[1]
      : s.points.find((p) => p[0] === hover)?.[1];

  return (
    <figure className={styles.figure} aria-labelledby={titleId}>
      <figcaption id={titleId} className={styles.title}>
        {title}
      </figcaption>
      {values.length === 0 ? (
        <p className={styles.empty}>{emptyText}</p>
      ) : (
        <>
          <svg
            className={styles.svg}
            viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
            role="img"
            aria-label={`${title}: ${series.map((s) => `${formatValue(readoutAt(s), unit)}${nameOf(s)}`).join(", ")}`}
            onPointerMove={onPointerMove}
            onPointerLeave={() => setHover(null)}
          >
            {yTicks(unit, low, high).map((tick) => (
              <g key={tick.value}>
                <line className={styles.grid} x1={MARGIN.left} x2={WIDTH - MARGIN.right} y1={y(tick.value)} y2={y(tick.value)} />
                <text className={styles.tick} x={MARGIN.left - 6} y={y(tick.value)} textAnchor="end" dominantBaseline="middle">
                  {tick.label}
                </text>
              </g>
            ))}
            {timeTicks(start, end).map((tick) => (
              <text key={tick.t} className={styles.tick} x={x(tick.t)} y={HEIGHT - 8} textAnchor="middle">
                {tick.label}
              </text>
            ))}
            <line className={styles.axis} x1={MARGIN.left} x2={WIDTH - MARGIN.right} y1={y(low)} y2={y(low)} />
            {referenceLines.map((line) => (
              <g key={`${line.severity}-${line.value}`} className={styles[line.severity]} data-testid="reference-line">
                <line x1={MARGIN.left} x2={WIDTH - MARGIN.right} y1={y(line.value)} y2={y(line.value)} />
                <text x={WIDTH - MARGIN.right - 4} y={y(line.value) - 4} textAnchor="end">
                  {line.label} {formatValue(line.value, unit)}
                </text>
              </g>
            ))}
            {markers
              .filter((m) => m.t >= start && m.t <= end)
              .map((m) => (
                <g key={m.label} className={styles.marker} data-testid="time-marker">
                  <line x1={x(m.t)} x2={x(m.t)} y1={MARGIN.top} y2={MARGIN.top + PLOT_H} />
                  <text x={x(m.t) + 4} y={MARGIN.top + 10}>
                    {m.label}
                  </text>
                </g>
              ))}
            {series.map((s, i) => (
              <g key={s.label} className={styles.series} style={{ color: `var(--series-${i + 1})` }}>
                {segments(s.points, step).map((segment) =>
                  segment.length === 1 ? (
                    <circle key={segment[0]![0]} cx={x(segment[0]![0])} cy={y(segment[0]![1])} r={2} />
                  ) : (
                    <path key={segment[0]![0]} d={segment.map(([t, v], j) => `${j ? "L" : "M"}${x(t)},${y(v)}`).join("")} />
                  ),
                )}
              </g>
            ))}
            {hover !== null && (
              <line className={styles.crosshair} x1={x(hover)} x2={x(hover)} y1={MARGIN.top} y2={MARGIN.top + PLOT_H} />
            )}
          </svg>
          <p className={styles.when}>
            {hover === null ? "Latest" : `At ${new Date(hover * 1000).toLocaleString("en", TIME_FORMAT)}`}
          </p>
          <ul className={styles.legend}>
            {series.map((s, i) => (
              <li key={s.label}>
                <span className={styles.key} style={{ background: `var(--series-${i + 1})` }} aria-hidden="true" />
                <strong>{formatValue(readoutAt(s), unit)}</strong>
                {nameOf(s)}
              </li>
            ))}
          </ul>
          {hidden > 0 && <p className={styles.when}>{hidden} more series not shown.</p>}
        </>
      )}
    </figure>
  );
}

const TIME_FORMAT: Intl.DateTimeFormatOptions = {
  month: "short",
  day: "numeric",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
};

/** The time in `sorted` closest to `t`. */
export function nearest(sorted: number[], t: number): number {
  let best = sorted[0]!;
  for (const candidate of sorted) {
    if (Math.abs(candidate - t) < Math.abs(best - t)) best = candidate;
  }
  return best;
}

/** Runs of points without nulls or gaps longer than twice `step`. */
export function segments(points: [number, number | null][], step?: number): [number, number][][] {
  const out: [number, number][][] = [];
  let current: [number, number][] = [];
  for (const [t, v] of points) {
    const previous = current[current.length - 1];
    if (v === null || (previous && step && t - previous[0] > 2 * step)) {
      if (current.length) out.push(current);
      current = [];
    }
    if (v !== null) current.push([t, v]);
  }
  if (current.length) out.push(current);
  return out;
}

/** Ratios always span 0–100%; other units start at zero unless negative. */
export function yDomain(unit: Unit, values: number[], lines: ReferenceLine[]): [number, number] {
  const all = [...values, ...lines.map((l) => l.value)];
  if (unit === "ratio") {
    return [Math.min(0, ...all), Math.max(1, ...all)];
  }
  const low = Math.min(0, ...all);
  const high = Math.max(...all, low);
  return high > low ? [low, high * 1.05 - low * 0.05] : [low, low + 1];
}

/** About five round values within `[low, high]`, labelled in `unit`. */
export function yTicks(unit: Unit, low: number, high: number): { value: number; label: string }[] {
  // Bytes get round values in their binary unit (512 MiB, not 536.9 MB).
  const exponent = unit === "bytes" ? binaryExponent(Math.max(Math.abs(low), Math.abs(high))) : 0;
  const scale = 1024 ** exponent;
  const raw = (high - low) / scale / 4;
  const magnitude = 10 ** Math.floor(Math.log10(raw));
  const size = ([1, 2, 2.5, 5, 10].find((m) => m * magnitude >= raw) ?? 10) * magnitude;
  const ticks = [];
  for (let v = Math.ceil(low / scale / size) * size; v <= high / scale + size * 1e-9; v += size) {
    const value = v * scale;
    ticks.push({ value, label: unit === "bytes" ? formatBytes(value, exponent) : formatValue(value, unit) });
  }
  return ticks;
}

const TICK_STEPS = [60, 300, 600, 900, 1800, 3600, 7200, 10_800, 21_600, 43_200, 86_400];

/** Round times within `[start, end]`, aligned to local time, about six of them. */
export function timeTicks(start: number, end: number): { t: number; label: string }[] {
  const size = TICK_STEPS.find((s) => (end - start) / s <= 7) ?? 86_400 * Math.ceil((end - start) / 86_400 / 7);
  const offset = -new Date(start * 1000).getTimezoneOffset() * 60;
  const ticks = [];
  for (let t = Math.ceil((start + offset) / size) * size - offset; t <= end; t += size) {
    const date = new Date(t * 1000);
    const label =
      size >= 86_400
        ? date.toLocaleDateString("en", { month: "short", day: "numeric" })
        : date.toLocaleTimeString("en", { hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
    ticks.push({ t, label });
  }
  return ticks;
}
