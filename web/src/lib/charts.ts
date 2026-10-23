import { ApiError, type Rollup, describeError } from "./api";
import { rollupFor } from "./metrics";

/**
 * Ranges and their steps, a few hundred points each (ADR-0020). Long ranges
 * summarize each step with a rollup instead of sampling it; short ones
 * show the samples as they are.
 */
export const RANGES = [
  { label: "1 h", seconds: 3_600, step: 30, rollup: false },
  { label: "6 h", seconds: 21_600, step: 120, rollup: false },
  { label: "24 h", seconds: 86_400, step: 300, rollup: true },
  { label: "7 d", seconds: 604_800, step: 1_800, rollup: true },
  { label: "30 d", seconds: 2_592_000, step: 7_200, rollup: true },
] as const;

export type Range = (typeof RANGES)[number];

/** The time span and step of one chart. */
export interface ChartWindow {
  start: number;
  end: number;
  step: number;
  rollup?: Rollup;
}

/**
 * The window of `metric` over `range` at `now` (unix seconds). With a
 * rollup, the step is at least the sensor's interval, rounded up to a
 * multiple of the range's step, so every step holds a sample (ADR-0018).
 * The end is aligned to the step, so refreshes return the same timestamps.
 */
export function chartWindow(range: Range, metric: string, intervalS: number, now: number): ChartWindow {
  const step = range.rollup ? Math.ceil(Math.max(intervalS, range.step) / range.step) * range.step : range.step;
  const end = Math.floor(now / step) * step;
  return { start: end - range.seconds, end, step, ...(range.rollup && { rollup: rollupFor(metric) }) };
}

/** What to tell the user about a failed metric request. */
export function metricsError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 502) return "The metrics backend is unavailable. Retrying every 15 seconds.";
    if (error.status === 504) return "The metrics backend timed out. Try a shorter range.";
  }
  return describeError(error);
}

/** The points of a chart as CSV, one column per series, for "Download CSV". */
export function seriesCsv(series: { label: string; points: [number, number | null][] }[]): string {
  const times = [...new Set(series.flatMap((s) => s.points.map((p) => p[0])))].sort((a, b) => a - b);
  const byTime = series.map((s) => new Map(s.points));
  const quote = (text: string) => (/[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text);
  const rows = times.map((t) =>
    [new Date(t * 1000).toISOString(), ...byTime.map((m) => String(m.get(t) ?? ""))].join(","),
  );
  return [["time", ...series.map((s) => quote(s.label))].join(","), ...rows].join("\n") + "\n";
}
