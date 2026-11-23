import { buckets, stackSeries, type StackedSeries } from "./chartScale";

/** How a chart over time draws its series (ADR-0178). */
export type ChartVariant = "line" | "area" | "columns";

/** Most columns drawn across a chart: more would be slivers. */
export const MAX_COLUMNS = 60;

/**
 * Areas and columns stack when there are several series that add up
 * (bytes, traffic, counts); percentages and temperatures overlap instead.
 */
export const stacks = (variant: ChartVariant, count: number, additive = true): boolean =>
  additive && variant !== "line" && count > 1;

/**
 * The series as drawn: columns gathered into at most {@link MAX_COLUMNS}
 * buckets, areas and columns stacked when there are several that add
 * up. `step` is the seconds between drawn points.
 */
export function shapeSeries(
  series: { label: string; points: [number, number | null][] }[],
  variant: ChartVariant,
  start: number,
  end: number,
  step: number | undefined,
  additive = true,
): { series: StackedSeries[]; step: number | undefined } {
  let drawnStep = step;
  const input =
    variant === "columns"
      ? series.map((s) => {
          const b = buckets(s.points, start, end, step ?? spacing(s.points), MAX_COLUMNS);
          drawnStep = b.width;
          return { label: s.label, points: b.points };
        })
      : series;
  return { series: stackSeries(input, stacks(variant, series.length, additive)), step: drawnStep };
}

/** The shortest time between two points, for series sent without a step. */
function spacing(points: [number, number | null][]): number {
  let least = Infinity;
  for (let i = 1; i < points.length; i++) least = Math.min(least, points[i]![0] - points[i - 1]![0]);
  return Number.isFinite(least) && least > 0 ? least : 1;
}

/** Runs of point indices without a missing value or, with a step, a gap over two steps. */
function runs(points: [number, number | null][], step: number | undefined): number[][] {
  const out: number[][] = [];
  let current: number[] = [];
  points.forEach(([t, v], i) => {
    const previous = current[current.length - 1];
    if (v === null || (previous !== undefined && step !== undefined && t - points[previous]![0] > 2 * step)) {
      if (current.length) out.push(current);
      current = [];
    }
    if (v !== null) current.push(i);
  });
  if (current.length) out.push(current);
  return out;
}

/**
 * The marks of shaped series in an SVG: lines, filled areas or columns,
 * each series in `color(i)`. `low` and `high` bound the y axis; `right` is
 * the x of the range's end, which columns do not cross.
 */
export function ChartMarks({
  series,
  variant,
  step,
  x,
  y,
  low,
  right,
  color,
  className,
  lineClassName,
}: {
  series: StackedSeries[];
  variant: ChartVariant;
  step: number | undefined;
  x: (t: number) => number;
  y: (v: number) => number;
  low: number;
  right: number;
  color: (index: number) => string;
  className?: (index: number) => string | undefined;
  lineClassName?: string;
}) {
  const floor = (v: number) => y(Math.max(v, low));
  // Stacked series sit on those before them; overlapping ones on the axis.
  const stacked = series.some((s) => s.base.some((b) => b !== 0));
  return (
    <>
      {series.map((s, i) => (
        <g key={s.label} className={className?.(i)} data-testid="chart-series">
          {variant === "columns"
            ? s.points.map(([t, v], k) => {
                if (v === null) return null;
                const width = x(t + (step ?? spacing(s.points))) - x(t);
                const left = x(t) + width * 0.12;
                const top = floor(v);
                return (
                  <rect
                    key={t}
                    x={left}
                    y={top}
                    width={Math.max(0, Math.min(width * 0.76, right - left))}
                    height={Math.max(0, floor(s.base[k]!) - top)}
                    fill={color(i)}
                    fillOpacity={stacked || series.length === 1 ? 1 : 0.5}
                  />
                );
              })
            : runs(s.points, step).map((run) => {
                const top = run.map((k) => `${x(s.points[k]![0])},${floor(s.points[k]![1]!)}`);
                return (
                  <g key={run[0]}>
                    {variant === "area" && (
                      <polygon
                        points={[...top, ...[...run].reverse().map((k) => `${x(s.points[k]![0])},${floor(s.base[k]!)}`)].join(" ")}
                        fill={color(i)}
                        fillOpacity={stacked ? 0.55 : series.length > 1 ? 0.12 : 0.22}
                        stroke="none"
                      />
                    )}
                    <polyline className={lineClassName} fill="none" stroke={color(i)} points={top.join(" ")} />
                  </g>
                );
              })}
        </g>
      ))}
    </>
  );
}
