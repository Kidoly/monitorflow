"use client";

import { useCallback, useState } from "react";

import { ApiError, type MetricInfo, type MetricSeries, api, describeError, isAbortError } from "@/lib/api";
import { unitOf } from "@/lib/format";
import { usePolling } from "@/lib/usePolling";

import { REFRESH_MS } from "./FleetOverview";
import styles from "./ServerGraphs.module.css";
import { MAX_SERIES, type ReferenceLine, TimeSeriesChart } from "./TimeSeriesChart";

/** Ranges and their steps, a few hundred points each (ADR-0020). */
export const RANGES = [
  { label: "1 h", seconds: 3_600, step: 30 },
  { label: "6 h", seconds: 21_600, step: 120 },
  { label: "24 h", seconds: 86_400, step: 300 },
  { label: "7 d", seconds: 604_800, step: 1_800 },
] as const;

type Range = (typeof RANGES)[number];

const PREFIX = "monitorflow_";

/** Built-in metrics (ADR-0015) in display order, with their titles. */
const KNOWN: Record<string, string> = {
  cpu_usage_ratio: "CPU usage",
  cpu_load1: "Load average, 1 min",
  cpu_load5: "Load average, 5 min",
  cpu_load15: "Load average, 15 min",
  memory_used_bytes: "Memory used",
  memory_available_ratio: "Memory available",
  memory_available_bytes: "Memory available (bytes)",
  memory_total_bytes: "Memory total",
  memory_swap_free_bytes: "Swap free",
  memory_swap_total_bytes: "Swap total",
  disk_used_ratio: "Disk used",
  disk_available_bytes: "Disk available",
  disk_free_bytes: "Disk free",
  disk_total_bytes: "Disk size",
  disk_inodes_free: "Free inodes",
  disk_inodes: "Inodes",
};

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
  const order = Object.keys(KNOWN);
  const rank = (m: MetricInfo) => {
    const i = order.indexOf(m.name.slice(PREFIX.length));
    return i === -1 ? order.length : i;
  };
  return [...metrics]
    .sort((a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name))
    .map((m) => {
      const short = m.name.slice(PREFIX.length);
      const total = short === "cpu_usage_ratio" && m.instances.includes("total");
      return { metric: m.name, title: KNOWN[short] ?? short, ...(total && { instance: "total" }) };
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
  start: number;
  end: number;
  truncated: boolean;
  charts: { plan: ChartPlan; result: ChartResult }[];
}

interface Props {
  serverId: string;
  /** Threshold lines for a chart, given the instances its series show. */
  referenceLines?: (plan: ChartPlan, instances: string[]) => ReferenceLine[];
}

/** Graphs of every metric the server wrote in the last day, with a range picker. */
export function ServerGraphs({ serverId, referenceLines }: Props) {
  const [range, setRange] = useState<Range>(RANGES[0]);
  const [data, setData] = useState<Loaded | null>(null);
  const [error, setError] = useState<string | null>(null);

  const load = useCallback(
    async (signal: AbortSignal) => {
      // Aligned to the step, so refreshes return the same timestamps.
      const end = Math.floor(Date.now() / 1000 / range.step) * range.step;
      const start = end - range.seconds;
      try {
        const { metrics, truncated } = await api.serverMetrics(serverId, signal);
        const plans = planCharts(metrics);
        const charts = await Promise.all(
          plans.map(async (plan) => {
            try {
              const { series } = await api.queryRange(
                {
                  serverId,
                  metric: plan.metric,
                  instance: plan.instance,
                  start: new Date(start * 1000),
                  end: new Date(end * 1000),
                  step: range.step,
                },
                signal,
              );
              return { plan, result: { series } };
            } catch (e) {
              if (isAbortError(e)) throw e;
              return { plan, result: { error: metricsError(e) } };
            }
          }),
        );
        setData({ range, start, end, truncated, charts });
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
          {data.charts.map(({ plan, result }) =>
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
                start={data.start}
                end={data.end}
                step={data.range.step}
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
