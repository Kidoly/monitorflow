// Made-up metric values: a daily cycle plus noise, worked out from the time
// alone so every chart, list and panel agrees, and so a reload draws the same
// curve. A few servers drift into the trouble their incidents describe.

import type { EffectiveSensor, MetricInfo, MetricSeries, SensorValue } from "../api";
import { primaryMetric } from "../sensors";

import { type DemoServer, isWindows } from "./fleet";
import { START } from "./time";

const PREFIX = "monitorflow_";
const GIB = 1024 ** 3;

/** A stable number from 0 to 1 for `text`. */
function hash(text: string): number {
  let h = 2166136261;
  for (let i = 0; i < text.length; i += 1) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return ((h >>> 0) % 100_000) / 100_000;
}

/** Smooth noise from -1 to 1: neighbouring minutes stay close. */
function noise(key: string, t: number): number {
  const minute = t / 60;
  const i = Math.floor(minute);
  const f = minute - i;
  const a = hash(`${key}:${i}`) * 2 - 1;
  const b = hash(`${key}:${i + 1}`) * 2 - 1;
  return a + (b - a) * (f * f * (3 - 2 * f));
}

/** 0 at night, 1 in the middle of the working day, in the viewer's time zone. */
function daily(t: number): number {
  const date = new Date(t * 1000);
  const hours = date.getHours() + date.getMinutes() / 60;
  return Math.max(0, Math.sin(((hours - 7) / 13) * Math.PI));
}

/** Weekends are quieter. */
function weekly(t: number): number {
  const day = new Date(t * 1000).getDay();
  return day === 0 || day === 6 ? 0.45 : 1;
}

const clamp = (value: number, low = 0, high = 1) => Math.min(high, Math.max(low, value));

const NOW_S = START / 1000;

/** When `app-04` stopped reporting, unix seconds. */
export const OFFLINE_SINCE = NOW_S - 47 * 60;

/** Seconds since `t` became part of the current trouble, or a negative number before. */
const since = (t: number, minutesAgo: number) => t - (NOW_S - minutesAgo * 60);

function cpu(server: DemoServer, t: number): number {
  const busy = daily(t) * weekly(t);
  let value = server.load * (0.45 + 0.75 * busy) + 0.06 * noise(`${server.id}:cpu`, t);
  if (server.hostname === "web-03" && since(t, 70) > 0) value = 0.87 + 0.04 * noise(`${server.id}:cpu-hot`, t);
  if (server.role === "backup" && new Date(t * 1000).getHours() === 2) value += 0.35;
  return clamp(value, 0.01, 0.99);
}

const MEMORY_TOTAL: Record<DemoServer["role"], number> = {
  web: 8, proxy: 4, db: 64, app: 16, mail: 8, files: 16, dc: 8, backup: 8, build: 32, cache: 16, misc: 4,
};

function memoryAvailable(server: DemoServer, t: number): number {
  let value = 0.62 - server.load * 0.4 - 0.08 * daily(t) + 0.03 * noise(`${server.id}:mem`, t);
  if (server.hostname === "mail-01" && since(t, 90) > 0) value = 0.085 + 0.015 * noise(`${server.id}:mem-low`, t);
  return clamp(value, 0.02, 0.95);
}

function diskUsed(server: DemoServer, disk: string, t: number): number {
  const base = 0.22 + 0.45 * hash(`${server.id}:${disk}`);
  // A slow growth over the month, as data piles up.
  const days = (t - NOW_S) / 86_400;
  let value = base + days * 0.002 + 0.002 * noise(`${server.id}:${disk}`, t / 30);
  if (server.hostname === "db-02" && disk === "/var/lib/postgresql") {
    // Filling up for a week, faster since yesterday.
    value = 0.9 + 0.06 * clamp(1 + days / 2) - 0.07 * clamp(-days / 7 - 0.3, 0, 1);
    value += 0.002 * noise(`${server.id}:pg`, t);
  }
  if (server.hostname === "backup-01" && disk === "/srv/backup") value = 0.81 + 0.03 * daily(t);
  return clamp(value, 0.05, 0.99);
}

const DISK_TOTAL_GIB = (disk: string) => (disk === "/" || disk === "C:" ? 40 : disk === "/srv/backup" ? 4000 : 500);

function diskBusy(server: DemoServer, t: number): number {
  let value = server.load * 0.4 * (0.4 + daily(t)) + 0.05 * noise(`${server.id}:io`, t);
  if (server.role === "backup" && new Date(t * 1000).getHours() === 2) value += 0.5;
  return clamp(value, 0.005, 0.98);
}

/** Whether the service is running at `t`. */
function serviceActive(server: DemoServer, service: string, t: number): number {
  if (server.hostname === "files-01" && service === "DFSR" && since(t, 38) > 0) return 0;
  return 1;
}

/** The instances `server` writes `metric` for; `[""]` for a metric without any. */
export function instancesOf(server: DemoServer, metric: string): string[] {
  const short = metric.startsWith(PREFIX) ? metric.slice(PREFIX.length) : metric;
  if (short.startsWith("disk_")) return server.disks;
  if (short.startsWith("diskio_")) return [isWindows(server) ? "PhysicalDisk0" : "sda"];
  if (short === "cpu_usage_ratio") return ["total"];
  if (short === "services_active") return server.services;
  if (short === "port_up") return server.role === "web" || server.role === "proxy" ? ["443"] : [];
  return [""];
}

/** The value of `metric` for one instance at `t` (unix seconds); null when nothing was written. */
export function valueAt(server: DemoServer, metric: string, instance: string, t: number): number | null {
  if (server.status === "offline" && t > OFFLINE_SINCE) return null;
  if (t > NOW_S + 60) return null;
  const short = metric.startsWith(PREFIX) ? metric.slice(PREFIX.length) : metric;
  const memTotal = MEMORY_TOTAL[server.role] * GIB;
  switch (short) {
    case "cpu_usage_ratio":
      return cpu(server, t);
    case "cpu_load1":
    case "cpu_load5":
    case "cpu_load15":
      return isWindows(server) ? null : cpu(server, t) * 4 * (short === "cpu_load1" ? 1.1 : short === "cpu_load5" ? 1 : 0.9);
    case "memory_available_ratio":
      return memoryAvailable(server, t);
    case "memory_available_bytes":
      return memoryAvailable(server, t) * memTotal;
    case "memory_used_bytes":
      return (1 - memoryAvailable(server, t)) * memTotal;
    case "memory_total_bytes":
      return memTotal;
    case "memory_swap_total_bytes":
      return 2 * GIB;
    case "memory_swap_free_bytes":
      return 2 * GIB * 0.93;
    case "disk_used_ratio":
      return diskUsed(server, instance, t);
    case "disk_total_bytes":
      return DISK_TOTAL_GIB(instance) * GIB;
    case "disk_available_bytes":
    case "disk_free_bytes":
      return (1 - diskUsed(server, instance, t)) * DISK_TOTAL_GIB(instance) * GIB;
    case "diskio_busy_ratio":
      return diskBusy(server, t);
    case "diskio_read_bytes_per_second":
      return diskBusy(server, t) * 90e6;
    case "diskio_written_bytes_per_second":
      return diskBusy(server, t) * 60e6;
    case "diskio_reads_per_second":
      return diskBusy(server, t) * 900;
    case "diskio_writes_per_second":
      return diskBusy(server, t) * 600;
    case "diskio_wait_seconds":
      return 0.002 + diskBusy(server, t) * 0.01;
    case "diskio_queue_depth":
      return diskBusy(server, t) * 2;
    case "services_active":
      return serviceActive(server, instance, t);
    case "port_up":
      return 1;
    default:
      return null;
  }
}

/** Points from `start` to `end` every `step` seconds, on multiples of `step`. */
export function points(server: DemoServer, metric: string, instance: string, start: number, end: number, step: number) {
  const out: [number, number | null][] = [];
  const first = Math.ceil(start / step) * step;
  for (let t = first; t <= end && out.length < 11_000; t += step) {
    out.push([t, valueAt(server, metric, instance, t)]);
  }
  return out;
}

/** Series of `metric` for one instance or all, like `GET /api/metrics/query_range`. */
export function rangeSeries(
  server: DemoServer,
  metric: string,
  instance: string | undefined,
  start: number,
  end: number,
  step: number,
): MetricSeries[] {
  const instances = instance === undefined ? instancesOf(server, metric) : [instance];
  return instances
    .filter((i) => valueAt(server, metric, i, Math.min(end, NOW_S)) !== null || server.status === "offline")
    .map((i) => ({
      labels: { __name__: metric, server_id: server.id, ...(i ? { instance: i } : {}) },
      points: points(server, metric, i, start, end, step),
    }));
}

/** What `server` checks: built-in sensors plus its services and ports. */
export function sensorsOf(server: DemoServer): EffectiveSensor[] {
  const sensor = (kind: string, name: string, instance = ""): EffectiveSensor => ({
    origin: kind === "systemd" || kind === "windows_service" || kind === "port" ? "site" : "default",
    entry_id: null,
    kind,
    name,
    instance,
    interval_ms: 60_000,
    timeout_ms: null,
    params: {},
  });
  const out = [sensor("cpu", "cpu"), sensor("memory", "memory"), sensor("disk", "disk"), sensor("diskio", "diskio")];
  const kind = isWindows(server) ? "windows_service" : "systemd";
  for (const service of server.services) out.push(sensor(kind, "services", service));
  if (instancesOf(server, "port_up").length) out.push(sensor("port", "port", "443"));
  return out;
}

/** The last hour of every sensor, as `GET /api/servers/{id}/sensor-values` gives it. */
export function sensorValues(server: DemoServer, now: number): SensorValue[] {
  const end = Math.floor(now / 60) * 60;
  return sensorsOf(server).map((s) => {
    const { metric, instance } = primaryMetric(s);
    const instances = instance !== undefined ? [instance] : instancesOf(server, metric);
    return {
      name: s.name,
      instance: s.instance,
      metric,
      reason: null,
      series: instances.map((i) => {
        const pts = points(server, metric, i, end - 3600, end, 60);
        const last = [...pts].reverse().find(([, v]) => v !== null);
        return {
          instance: i,
          points: pts,
          last_value: last ? last[1] : null,
          last_at: last ? new Date(last[0] * 1000).toISOString() : null,
          reason: server.status === "offline" ? "No data for 47 minutes" : null,
        };
      }),
    };
  });
}

const BUILTIN = [
  "cpu_usage_ratio", "cpu_load1", "cpu_load5", "cpu_load15",
  "memory_used_bytes", "memory_available_ratio", "memory_available_bytes", "memory_total_bytes",
  "memory_swap_free_bytes", "memory_swap_total_bytes",
  "disk_used_ratio", "disk_available_bytes", "disk_free_bytes", "disk_total_bytes",
  "diskio_busy_ratio", "diskio_wait_seconds", "diskio_read_bytes_per_second", "diskio_written_bytes_per_second",
  "diskio_reads_per_second", "diskio_writes_per_second", "diskio_queue_depth",
  "services_active", "port_up",
];

/** Metrics `server` wrote in the last day. */
export function metricsOf(server: DemoServer): MetricInfo[] {
  return BUILTIN.map((short) => `${PREFIX}${short}`)
    .filter((metric) => instancesOf(server, metric).length > 0 && valueAt(server, metric, instancesOf(server, metric)[0]!, NOW_S - 3600) !== null)
    .map((metric) => ({ name: metric, instances: instancesOf(server, metric).filter(Boolean) }));
}

/** Every metric the demo writes, for the dashboard editor. */
export const ALL_METRICS = BUILTIN.map((short) => `${PREFIX}${short}`);
