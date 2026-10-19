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
