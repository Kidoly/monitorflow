import { type PlainKey, t } from "@/i18n";

import { METRIC_PREFIX } from "./metrics";

/** One chart of a disk's activity: its title and its series, as metric suffixes and legend labels. */
export interface DiskIoChart {
  title: PlainKey;
  series: { suffix: string; label: PlainKey }[];
}

/**
 * What a disk's page charts besides its own metric (ADR-0092): how much it
 * moves, how often, how busy it is, how long an I/O takes and how many wait.
 */
export const DISK_IO_CHARTS: DiskIoChart[] = [
  {
    title: "server.diskio.readWritten",
    series: [
      { suffix: "read_bytes_per_second", label: "server.diskio.read" },
      { suffix: "written_bytes_per_second", label: "server.diskio.written" },
    ],
  },
  {
    title: "server.diskio.readsWrites",
    series: [
      { suffix: "reads_per_second", label: "server.diskio.reads" },
      { suffix: "writes_per_second", label: "server.diskio.writes" },
    ],
  },
  { title: "server.diskio.busy", series: [{ suffix: "busy_ratio", label: "server.diskio.busy" }] },
  { title: "server.diskio.waitTitle", series: [{ suffix: "wait_seconds", label: "server.diskio.wait" }] },
  { title: "server.diskio.inFlight", series: [{ suffix: "queue_depth", label: "server.diskio.queue" }] },
];

/**
 * The charts to show under the chart of `metric`, a metric of the diskio
 * sensor `name`, with full metric names: every chart but the one that is
 * `metric` alone.
 */
export function diskIoCharts(name: string, metric: string): { title: string; series: { metric: string; label: string }[] }[] {
  return DISK_IO_CHARTS.map((chart) => ({
    title: t(chart.title),
    series: chart.series.map((s) => ({ metric: `${METRIC_PREFIX}${name}_${s.suffix}`, label: t(s.label) })),
  })).filter((chart) => !(chart.series.length === 1 && chart.series[0]!.metric === metric));
}
