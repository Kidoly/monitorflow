"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import {
  ApiError,
  type EffectiveSensor,
  type MetricInfo,
  type MetricSeries,
  type Rollup,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { unitOf } from "@/lib/format";
import { DEFAULT_INTERVAL_S, METRIC_TITLES, metricTitle, rollupFor, sensorInterval, shortMetric } from "@/lib/metrics";
import { usePolling } from "@/lib/usePolling";

import { REFRESH_MS } from "./FleetOverview";
import styles from "./ServerGraphs.module.css";
import { MAX_SERIES, type ReferenceLine, TimeSeriesChart } from "./TimeSeriesChart";

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

/** One chart: a metric, all its instances or only one. */
export interface ChartPlan {
  metric: string;
  instance?: string;
  title: string;
}

/**
 * Charts for the discovered metrics: built-in ones first in a fixed order,
 * then the others by name. CPU usage shows the `total` instance only; one
 * line per core is too many for one chart.
 */
export function planCharts(metrics: MetricInfo[]): ChartPlan[] {
  const order = Object.keys(METRIC_TITLES);
  const rank = (m: MetricInfo) => {
    const i = order.indexOf(shortMetric(m.name));
    return i === -1 ? order.length : i;
  };
  return [...metrics]
    .sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name))
    .map((m) => {
      const total = shortMetric(m.name) === "cpu_usage_ratio" && m.instances.includes("total");
      return { metric: m.name, title: metricTitle(m.name), ...(total && { instance: "total" }) };
    });
}

/** What to tell the user about a failed metric request. */
export function metricsError(error: unknown): string {
  if (error instanceof ApiError) {
    if (error.status === 502) return "The metrics backend is unavailable. Graphs retry every 15 seconds.";
    if (error.status === 504) return "The metrics backend timed out. Try a shorter range.";
  }
  return describeError(error);
}

type ChartResult = { series: MetricSeries[] } | { error: string };

interface Loaded {
  range: Range;
  truncated: boolean;
  charts: { plan: ChartPlan; window: ChartWindow; result: ChartResult }[];
}

interface Props {
  serverId: string;
  /** The effective sensors, for their intervals; the default interval until known. */
  sensors?: EffectiveSensor[];
  /** Threshold lines for a chart, given the instances its series show. */
  referenceLines?: (plan: ChartPlan, instances: string[]) => ReferenceLine[];
}

/** Graphs of every metric the server wrote in the last day, with a range picker. */
export function ServerGraphs({ serverId, sensors, referenceLines }: Props) {
  const [range, setRange] = useState<Range>(RANGES[0]);
  const [data, setData] = useState<Loaded | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Read at the next refresh: a new sensor list does not reload the graphs.
  const sensorsRef = useRef(sensors);
  useEffect(() => {
    sensorsRef.current = sensors;
  }, [sensors]);

  const load = useCallback(
    async (signal: AbortSignal) => {
      const now = Date.now() / 1000;
      try {
        const { metrics, truncated } = await api.serverMetrics(serverId, signal);
        const plans = planCharts(metrics);
        const charts = await Promise.all(
          plans.map(async (plan) => {
            const interval = sensorInterval(sensorsRef.current ?? [], plan.metric) ?? DEFAULT_INTERVAL_S;
            const window = chartWindow(range, plan.metric, interval, now);
            try {
              const { series } = await api.queryRange(
                {
                  serverId,
                  metric: plan.metric,
                  instance: plan.instance,
                  start: new Date(window.start * 1000),
                  end: new Date(window.end * 1000),
                  step: window.step,
                  rollup: window.rollup,
                },
                signal,
              );
              return { plan, window, result: { series } };
            } catch (e) {
              if (isAbortError(e)) throw e;
              return { plan, window, result: { error: metricsError(e) } };
            }
          }),
        );
        setData({ range, truncated, charts });
        setError(null);
      } catch (e) {
        // A 401 has already been routed to the login page.
        if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) {
          setError(metricsError(e));
        }
      }
    },
    [serverId, range],
  );
  usePolling(load, REFRESH_MS);

  return (
    <section className={styles.section} aria-labelledby="graphs-title">
      <div className={styles.heading}>
        <h2 id="graphs-title">Metrics</h2>
        <div className={styles.ranges} role="group" aria-label="Time range">
          {RANGES.map((r) => (
            <button
              key={r.label}
              type="button"
              className={styles.range}
              aria-pressed={r === range}
              onClick={() => setRange(r)}
            >
              {r.label}
            </button>
          ))}
        </div>
      </div>
      {error && (
        <p className={styles.error} role="alert">
          {data ? `Refresh failed: ${error} Showing the last data received.` : error}
        </p>
      )}
      {!data && !error && <p role="status">Loading metrics…</p>}
      {data?.charts.length === 0 && (
        <p className={styles.empty}>
          No metrics yet. An agent sends its first samples one collection interval (60 seconds by
          default) after it connects, and new points take about 30 seconds more to be queryable.
        </p>
      )}
      {data?.truncated && (
        <p className={styles.note}>The server has too many series to list; some metrics may be missing.</p>
      )}
      {data && data.charts.length > 0 && (
        <div className={styles.grid} aria-busy={data.range !== range}>
          {data.charts.map(({ plan, window, result }) =>
            "error" in result ? (
              <figure key={plan.metric} className={styles.failed}>
                <figcaption>{plan.title}</figcaption>
                <p role="alert">{result.error}</p>
              </figure>
            ) : (
              <TimeSeriesChart
                key={plan.metric}
                title={plan.title}
                unit={unitOf(plan.metric)}
                start={window.start}
                end={window.end}
                step={window.step}
                series={result.series.map((s) => ({ label: s.labels.instance || plan.title, points: s.points }))}
                referenceLines={referenceLines?.(
                  plan,
                  result.series.slice(0, MAX_SERIES).map((s) => s.labels.instance ?? ""),
                )}
                emptyText="No data in this range yet. New points take about 30 seconds to appear."
              />
            ),
          )}
        </div>
      )}
    </section>
  );
}
