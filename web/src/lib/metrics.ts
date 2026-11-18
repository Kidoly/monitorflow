import { t } from "@/i18n";

import type { EffectiveSensor, Rollup } from "./api";

/** Prefix of every metric MonitorFlow stores (ADR-0015). */
export const METRIC_PREFIX = "monitorflow_";

/** Built-in metrics (ADR-0015) in display order; their titles are in the catalog. */
export const BUILTIN_METRICS = [
  "cpu_usage_ratio",
  "cpu_load1",
  "cpu_load5",
  "cpu_load15",
  "memory_used_bytes",
  "memory_available_ratio",
  "memory_available_bytes",
  "memory_total_bytes",
  "memory_swap_free_bytes",
  "memory_swap_total_bytes",
  "disk_used_ratio",
  "disk_available_bytes",
  "disk_free_bytes",
  "disk_total_bytes",
  "disk_inodes_free",
  "disk_inodes",
  "diskio_busy_ratio",
  "diskio_wait_seconds",
  "diskio_read_bytes_per_second",
  "diskio_written_bytes_per_second",
  "diskio_reads_per_second",
  "diskio_writes_per_second",
  "diskio_queue_depth",
  "ping_up",
  "ping_loss_ratio",
  "ping_rtt_seconds",
  "snmp_up",
  "snmp_uptime_seconds",
  "snmp_if_oper_up",
  "snmp_if_admin_up",
  "snmp_if_in_bytes_per_second",
  "snmp_if_out_bytes_per_second",
  "snmp_if_in_errors_per_second",
  "snmp_if_out_errors_per_second",
  "snmp_if_in_discards_per_second",
  "snmp_if_out_discards_per_second",
  "snmp_if_in_utilization_ratio",
  "snmp_if_out_utilization_ratio",
  "snmp_if_speed_bits_per_second",
  "snmp_host_cpu_usage_ratio",
  "snmp_host_memory_used_ratio",
  "snmp_host_storage_used_ratio",
  "snmp_host_storage_used_bytes",
  "snmp_host_storage_size_bytes",
  "snmp_ups_battery_charge_ratio",
  "snmp_ups_runtime_seconds",
  "snmp_ups_on_battery",
  "snmp_ups_battery_low",
  "snmp_temp_celsius",
  "snmp_printer_supply_level_ratio",
] as const;

type BuiltinMetric = (typeof BUILTIN_METRICS)[number];

const isBuiltin = (name: string): name is BuiltinMetric => (BUILTIN_METRICS as readonly string[]).includes(name);

/** The metric's name without the prefix: `monitorflow_port_open` gives `port_open`. */
export function shortMetric(metric: string): string {
  return metric.startsWith(METRIC_PREFIX) ? metric.slice(METRIC_PREFIX.length) : metric;
}

/** A readable title: a built-in metric's, else its short name. */
export function metricTitle(metric: string): string {
  const short = shortMetric(metric);
  return isBuiltin(short) ? t(`server.metric.${short}`) : short;
}

/** Collection interval when the sensor's configuration is not known (ADR-0016). */
export const DEFAULT_INTERVAL_S = 60;

/**
 * How a long range summarizes `metric` per step (ADR-0020): the worst value
 * of the step. Headroom and health (`available`, `free`, `_up`, `_active`,
 * `_loaded`, `_installed`) take the minimum, so a dip or an outage shows; usage and
 * pressure (ratios, used bytes, load, durations, queues, statuses, failures,
 * matches, lag, restarts) take the maximum, so a spike shows. Sizes and
 * custom gauges of unknown meaning take the average.
 */
export function rollupFor(metric: string): Rollup {
  const short = shortMetric(metric);
  if (/available|free|_(up|active|loaded|installed)$/.test(short)) return "min";
  if (/(_ratio|_used_bytes|_seconds|_status|_failed|_matches|_lag_bytes|_lag_events|_restarts|_queue_depth)$|^cpu_load/.test(short)) {
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
