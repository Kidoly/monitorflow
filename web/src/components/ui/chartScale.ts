const NICE = [1, 2, 2.5, 5, 10];

/**
 * Five evenly spaced round values covering `[min, max]`, the y axis of
 * every chart: 0 to 100 gives 0, 25, 50, 75, 100.
 */
export function niceTicks(min: number, max: number, count = 5): number[] {
  if (!(max > min)) max = min + 1;
  const intervals = count - 1;
  const raw = (max - min) / intervals;
  let magnitude = 10 ** Math.floor(Math.log10(raw));
  for (;;) {
    for (const nice of NICE) {
      const step = nice * magnitude;
      const low = Math.floor(min / step + 1e-9) * step;
      if (low + intervals * step >= max - 1e-9) {
        // Rounding noise would print 0.30000000000000004.
        return Array.from({ length: count }, (_, i) => Number((low + i * step).toPrecision(12)));
      }
    }
    magnitude *= 10;
  }
}

import { dateLocale } from "@/i18n/locale";

const pad = (n: number) => String(n).padStart(2, "0");

/**
 * Seven x ticks, every sixth of `[start, end]` (unix seconds), labelled
 * HH:MM up to 24 h and DD MMM beyond, the month in the active language.
 */
export function timeTicks(start: number, end: number): { t: number; label: string }[] {
  const daily = end - start > 86_400;
  return Array.from({ length: 7 }, (_, i) => {
    const t = start + ((end - start) * i) / 6;
    const d = new Date(t * 1000);
    return { t, label: daily ? d.toLocaleDateString(dateLocale(), { day: "2-digit", month: "short" }) : `${pad(d.getHours())}:${pad(d.getMinutes())}` };
  });
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

/** Index in the sorted `times` of the one closest to `t`. */
export function nearestIndex(times: number[], t: number): number {
  let best = 0;
  times.forEach((candidate, i) => {
    if (Math.abs(candidate - t) < Math.abs(times[best]! - t)) best = i;
  });
  return best;
}

/** A series stacked on those before it: each point's top, and the base it sits on. */
export interface StackedSeries {
  label: string;
  points: [number, number | null][];
  /** The base under each point, by index. */
  base: number[];
}

/**
 * `series` stacked in order, time by time: a point's top is its value plus
 * the tops of the series before it at that time; a missing value adds
 * nothing and draws nothing. Unstacked, every base is `floor`.
 */
export function stackSeries(series: { label: string; points: [number, number | null][] }[], stacked: boolean, floor = 0): StackedSeries[] {
  const running = new Map<number, number>();
  return series.map((s) => {
    const base: number[] = [];
    const points = s.points.map(([t, v]): [number, number | null] => {
      const under = stacked ? (running.get(t) ?? 0) : floor;
      base.push(under);
      if (v === null) return [t, null];
      const top = stacked ? under + v : v;
      if (stacked) running.set(t, top);
      return [t, top];
    });
    return { label: s.label, points, base };
  });
}

/**
 * `points` gathered into at most `count` buckets of equal time from
 * `start`, each the average of its values (null when it has none), so
 * columns stay wide enough to read. Returns the points and the bucket's
 * length in seconds.
 */
export function buckets(
  points: [number, number | null][],
  start: number,
  end: number,
  step: number,
  count: number,
): { points: [number, number | null][]; width: number } {
  const width = Math.max(step, Math.ceil((end - start) / count / step) * step);
  if (width === step) return { points, width };
  const sums = new Map<number, [number, number]>();
  for (const [t, v] of points) {
    const at = start + Math.floor((t - start) / width) * width;
    const sum = sums.get(at) ?? [0, 0];
    if (v !== null) sums.set(at, [sum[0] + v, sum[1] + 1]);
    else sums.set(at, sum);
  }
  return {
    points: [...sums.entries()].sort((a, b) => a[0] - b[0]).map(([t, [total, n]]) => [t, n ? total / n : null]),
    width,
  };
}
