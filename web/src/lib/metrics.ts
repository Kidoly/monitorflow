import type { EffectiveSensor, Rollup } from "./api";

/** Prefix of every metric MonitorFlow stores (ADR-0015). */
export const METRIC_PREFIX = "monitorflow_";

/** Built-in metrics (ADR-0015) in display order, with their titles. */
export const METRIC_TITLES: Record<string, string> = {
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

/** The metric's name without the prefix: `monitorflow_port_open` gives `port_open`. */
export function shortMetric(metric: string): string {
  return metric.startsWith(METRIC_PREFIX) ? metric.slice(METRIC_PREFIX.length) : metric;
}

/** A readable title: a built-in metric's, else its short name. */
export function metricTitle(metric: string): string {
  const short = shortMetric(metric);
  return METRIC_TITLES[short] ?? short;
}

/** Collection interval when the sensor's configuration is not known (ADR-0016). */
export const DEFAULT_INTERVAL_S = 60;

/**
 * How a long range summarizes `metric` per step (ADR-0020): the worst value
 * of the step. Headroom and health (`available`, `free`, `_up`, `_active`,
 * `_loaded`, `_installed`) take the minimum, so a dip or an outage shows; usage and
 * pressure (ratios, used bytes, load, durations, statuses, failures,
 * matches, lag, restarts) take the maximum, so a spike shows. Sizes and
 * custom gauges of unknown meaning take the average.
 */
export function rollupFor(metric: string): Rollup {
  const short = shortMetric(metric);
  if (/available|free|_(up|active|loaded|installed)$/.test(short)) return "min";
  if (/(_ratio|_used_bytes|_seconds|_status|_failed|_matches|_lag_bytes|_lag_events|_restarts)$|^cpu_load/.test(short)) {
    return "max";
  }
  return "avg";
}

/**
 * The longest collection interval, in seconds, of the sensors that write
 * `metric`: the ones with the longest name that prefixes it
 * (`monitorflow_disk_data_used_ratio` comes from `disk_data`, not `disk`).
 * Undefined when no sensor matches.
 */
export function sensorInterval(sensors: EffectiveSensor[], metric: string): number | undefined {
  const short = shortMetric(metric);
  const matching = sensors.filter((s) => short.startsWith(`${s.name}_`));
  const longest = Math.max(...matching.map((s) => s.name.length));
  const intervals = matching.filter((s) => s.name.length === longest).map((s) => s.interval_ms / 1000);
  return intervals.length > 0 ? Math.max(...intervals) : undefined;
}
