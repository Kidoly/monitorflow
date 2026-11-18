import { t } from "@/i18n";

import type { EffectiveSensor, Incident, SensorSeries, SensorValueList } from "./api";
import { checkTitle } from "./alerts";
import { formatSeconds, formatValue, unitOf } from "./format";
import { METRIC_PREFIX, metricTitle, shortMetric } from "./metrics";
import { websiteHost } from "./website";

/**
 * The metric that stands for a sensor in the server's sensor list, by
 * kind (ADR-0015): its suffix, and the one instance to keep when the
 * sensor writes several (CPU usage per core and in total).
 */
const PRIMARY: Record<string, { suffix: string; instance?: string }> = {
  cpu: { suffix: "usage_ratio", instance: "total" },
  memory: { suffix: "available_ratio" },
  disk: { suffix: "used_ratio" },
  diskio: { suffix: "busy_ratio" },
  systemd: { suffix: "active" },
  windows_service: { suffix: "active" },
  port: { suffix: "up" },
  ping: { suffix: "up" },
  log: { suffix: "matches" },
  eventlog: { suffix: "matches" },
  script: { suffix: "status" },
  http: { suffix: "up" },
};

/** The full name of the metric shown for `sensor`, and the instance to query, if any. */
export function primaryMetric(sensor: EffectiveSensor): { metric: string; instance?: string } {
  const primary = PRIMARY[sensor.kind];
  const metric = `${METRIC_PREFIX}${sensor.name}${primary ? `_${primary.suffix}` : ""}`;
  const instance = sensor.instance || primary?.instance;
  return instance ? { metric, instance } : { metric };
}

/**
 * The sensor that writes `metric`: the one with the longest name that
 * prefixes it (`disk_data_used_ratio` comes from `disk_data`, not `disk`).
 */
export function sensorOf(sensors: EffectiveSensor[], metric: string): EffectiveSensor | undefined {
  const short = shortMetric(metric);
  return sensors
    .filter((s) => short === s.name || short.startsWith(`${s.name}_`))
    .sort((a, b) => b.name.length - a.name.length)[0];
}

/** A row's title: "Disk used /var", "nginx.service", "Port 443". */
export function seriesTitle(sensor: EffectiveSensor | undefined, metric: string, instance: string): string {
  const shown = instance === "total" ? "" : instance;
  const primary = sensor && primaryMetric(sensor).metric === metric;
  // A check from the central server or a relay says which (ADR-0148).
  if (sensor && primary && sensor.probe) return checkTitle(sensor);
  if (sensor && primary) {
    switch (sensor.kind) {
      case "cpu":
        return t("sensors.title.cpu");
      case "memory":
        return t("sensors.title.memory");
      case "disk":
        return shown ? t("sensors.title.diskOn", { instance: shown }) : t("sensors.title.disk");
      case "diskio":
        return shown ? t("sensors.title.diskioOn", { instance: shown }) : t("sensors.title.diskio");
      case "systemd":
      case "windows_service":
        return shown || sensor.name;
      case "port":
        return t("sensors.title.port", { port: shown || sensor.name });
      case "http":
        return t("sensors.title.website", { host: websiteHost(sensor.params.url) || sensor.name });
      case "log":
      case "eventlog":
      case "script":
        return sensor.name;
    }
  }
  return `${metricTitle(metric)}${shown ? ` ${shown}` : ""}`;
}

/** A value as a sensor list shows it: unit states in words, others in their unit. */
export function formatSensorValue(metric: string, value: number | null | undefined): string {
  if (value === null || value === undefined) return "–";
  const short = shortMetric(metric);
  if (short.endsWith("_active")) return value === 1 ? t("sensors.value.active") : t("sensors.value.inactive");
  if (short.startsWith("ping") && short.endsWith("_up")) return value === 1 ? t("sensors.value.answers") : t("sensors.value.noAnswer");
  // A website check: "http_shop_example_com_up" (ADR-0150).
  if (short.startsWith("http_") && short.endsWith("_up")) return value === 1 ? t("sensors.value.answers") : t("sensors.value.fails");
  if (short.endsWith("_up")) return value === 1 ? t("sensors.value.open") : t("sensors.value.closed");
  if (short.endsWith("_loaded")) return value === 1 ? t("sensors.value.loaded") : t("sensors.value.notLoaded");
  if (short.endsWith("_installed")) return value === 1 ? t("sensors.value.installed") : t("sensors.value.notInstalled");
  return formatValue(value, unitOf(metric));
}

/** "disk · /var · every 30 s · timeout 10 s": a sensor with its timing. */
export function sensorFacts(sensor: EffectiveSensor, instance: string): string {
  return [
    sensor.kind,
    instance && instance !== "total" ? instance : sensor.instance,
    t("sensors.facts.every", { interval: formatSeconds(sensor.interval_ms / 1000) }),
    sensor.timeout_ms !== null && t("sensors.facts.timeout", { interval: formatSeconds(sensor.timeout_ms / 1000) }),
  ]
    .filter(Boolean)
    .join(" · ");
}

export type SensorStatus = "critical" | "warning" | "unknown" | "ok";

/** One line of the server's sensor list. */
export interface SensorRow {
  key: string;
  sensor: EffectiveSensor;
  /** The metric shown: the sensor's primary one, or the one an incident is open on. */
  metric: string;
  instance: string;
  title: string;
  value: number | null;
  /** The last hour of the shown metric; empty when unknown. */
  points: SensorSeries["points"];
  /** Why the sensor is not OK, in the server's words: an incident's reason or late data. */
  reason: string | null;
  /** Why there is no value: the metrics backend failed. */
  error: string | null;
  /** No sample yet, as for a check just added: not a failure. */
  waiting: boolean;
  /** The worst open incident on the sensor's metrics for this instance. */
  incident: Incident | undefined;
  status: SensorStatus;
}

/** The recent values of a sensor's primary metric, or why they could not be read. */
export type SensorValues = { series: SensorSeries[]; reason: string | null } | { error: string };

/** Each sensor's entry of `list`, by name and configured instance. */
export function valuesBySensor(sensors: EffectiveSensor[], list: SensorValueList): Map<EffectiveSensor, SensorValues> {
  return new Map(
    sensors.map((sensor) => {
      const found = list.sensors.find((v) => v.name === sensor.name && v.instance === sensor.instance);
      return [sensor, found ? { series: found.series, reason: found.reason } : { series: [], reason: null }];
    }),
  );
}

const RANK: Record<SensorStatus, number> = { critical: 0, warning: 1, unknown: 2, ok: 3 };
const byTitle = new Intl.Collator("en", { numeric: true });

/** The worse of two open incidents: critical first, then the older one. */
function worse(a: Incident | undefined, b: Incident): Incident {
  if (!a) return b;
  if (a.severity !== b.severity) return a.severity === "critical" ? a : b;
  return Date.parse(a.opened_at) <= Date.parse(b.opened_at) ? a : b;
}

/**
 * The sensor list: one row per sensor and instance of its primary metric
 * (one per mount for a disk sensor), with the worst open incident of the
 * sensor's metrics on that instance. Sorted by status, then title.
 */
export function sensorRows(
  sensors: EffectiveSensor[],
  values: Map<EffectiveSensor, SensorValues>,
  openIncidents: Incident[],
): SensorRow[] {
  const rows: SensorRow[] = [];
  for (const sensor of sensors) {
    const { metric, instance: queried } = primaryMetric(sensor);
    const result = values.get(sensor);
    const series = result && "series" in result ? result.series : [];
    const incidents = openIncidents.filter((i) => sensorOf(sensors, i.metric) === sensor);
    const instances = series.length > 0 ? series.map((s) => s.instance) : [queried ?? ""];
    for (const instance of instances) {
      const shown = series.find((s) => s.instance === instance);
      const value = shown?.last_value ?? null;
      // A sensor with one row owns all its incidents (load average under CPU usage).
      const own = instances.length === 1 ? incidents : incidents.filter((i) => i.instance === instance);
      const incident = own.reduce<Incident | undefined>(worse, undefined);
      const error = result && "error" in result ? result.error : null;
      const late = shown ? shown.reason : result && "reason" in result ? result.reason : null;
      rows.push({
        key: `${sensor.name}/${sensor.instance}/${instance}`,
        sensor,
        metric: incident?.metric ?? metric,
        instance: incident?.instance ?? instance,
        title: seriesTitle(sensor, metric, instance),
        value: incident ? incident.last_value : value,
        points: shown?.points ?? [],
        reason: incident ? (incident.reason ?? null) : late,
        error: incident ? null : error,
        // Never reported: no series at all, as for a check just added.
        waiting: !incident && !error && value === null && !shown,
        incident,
        status: incident ? incident.severity : error || late || value === null ? "unknown" : "ok",
      });
    }
  }
  return rows.sort((a, b) => RANK[a.status] - RANK[b.status] || byTitle.compare(a.title, b.title));
}
