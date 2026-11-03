import { METRIC_PREFIX } from "./metrics";

/** One chart of a disk's activity: its title and its series, as metric suffixes and legend labels. */
export interface DiskIoChart {
  title: string;
  series: { suffix: string; label: string }[];
}

/**
 * What a disk's page charts besides its own metric (ADR-0092): how much it
 * moves, how often, how busy it is, how long an I/O takes and how many wait.
 */
export const DISK_IO_CHARTS: DiskIoChart[] = [
  {
    title: "Read and written",
    series: [
      { suffix: "read_bytes_per_second", label: "Read" },
      { suffix: "written_bytes_per_second", label: "Written" },
    ],
  },
  {
    title: "Reads and writes",
    series: [
      { suffix: "reads_per_second", label: "Reads" },
      { suffix: "writes_per_second", label: "Writes" },
    ],
  },
  { title: "Busy", series: [{ suffix: "busy_ratio", label: "Busy" }] },
  { title: "Average wait per I/O", series: [{ suffix: "wait_seconds", label: "Wait" }] },
  { title: "I/Os in flight", series: [{ suffix: "queue_depth", label: "Queue" }] },
];

/**
 * The charts to show under the chart of `metric`, a metric of the diskio
 * sensor `name`, with full metric names: every chart but the one that is
 * `metric` alone.
 */
export function diskIoCharts(name: string, metric: string): { title: string; series: { metric: string; label: string }[] }[] {
  return DISK_IO_CHARTS.map((chart) => ({
    title: chart.title,
    series: chart.series.map((s) => ({ metric: `${METRIC_PREFIX}${name}_${s.suffix}`, label: s.label })),
  })).filter((chart) => !(chart.series.length === 1 && chart.series[0]!.metric === metric));
}
