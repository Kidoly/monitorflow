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

const pad = (n: number) => String(n).padStart(2, "0");
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/**
 * Seven x ticks, every sixth of `[start, end]` (unix seconds), labelled
 * HH:MM up to 24 h and DD MMM beyond.
 */
export function timeTicks(start: number, end: number): { t: number; label: string }[] {
  const daily = end - start > 86_400;
  return Array.from({ length: 7 }, (_, i) => {
    const t = start + ((end - start) * i) / 6;
    const d = new Date(t * 1000);
    return { t, label: daily ? `${pad(d.getDate())} ${MONTHS[d.getMonth()]}` : `${pad(d.getHours())}:${pad(d.getMinutes())}` };
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
