import { t } from "@/i18n";

import {
  type ConfigOwner,
  type EffectiveSensor,
  type EffectiveThreshold,
  type SensorConfigEntry,
  type ThresholdRequest,
  api,
} from "./api";
import { type Shape, saveThreshold } from "./alerts";
import { formatNumber, formatPercent, formatSeconds } from "./format";
import { every } from "./intervals";
import { primaryMetric, sensorOf } from "./sensors";

/**
 * One sensor's rule (ADR-0016, ADR-0017): how often it runs, its timeout
 * and its alert, and the level each comes from. Levels stack as Built-in,
 * then the site's default, then the server's, then one instance of the
 * server's, like one disk; the most specific wins.
 */
export type RuleLevel = "builtin" | "site" | "server" | "sensor";

const RANK: Record<RuleLevel, number> = { builtin: 0, site: 1, server: 2, sensor: 3 };

/** The source badge of a rule, in the active language: read when shown. */
export const LEVEL_LABELS: Record<RuleLevel, string> = {
  get builtin() {
    return t("sensors.level.builtin");
  },
  get site() {
    return t("sensors.level.site");
  },
  get server() {
    return t("sensors.level.server");
  },
  get sensor() {
    return t("sensors.level.sensor");
  },
};

/**
 * Whether `instance` is one of several the sensor measures, like one disk
 * of a disk sensor, so a rule can single it out. The one instance a sensor
 * shows (CPU usage's `total`) is not.
 */
export function perInstance(sensor: EffectiveSensor, instance: string): boolean {
  return instance !== "" && sensor.instance === "" && primaryMetric(sensor).instance !== instance;
}

/**
 * The threshold the engine applies to `metric` on `instance`, muted ones
 * included: the exact instance's wins over the one for every instance.
 */
export function ruleOf(thresholds: EffectiveThreshold[], metric: string, instance: string): EffectiveThreshold | undefined {
  // The plain threshold: richer rules of the same metric are listed on the alerts panel (ADR-0153).
  const ofMetric = thresholds.filter((t) => t.metric === metric && (t.kind ?? "value") === "value");
  return ofMetric.find((t) => t.instance === instance) ?? ofMetric.find((t) => t.instance === null);
}

/**
 * The rule a sensor row shows: the threshold on the metric it shows, else
 * on the sensor's primary metric, else on any metric of the sensor (disk
 * activity alerts on its wait, not on how busy it is).
 */
export function rowRule(
  sensors: EffectiveSensor[],
  thresholds: EffectiveThreshold[],
  sensor: EffectiveSensor,
  metric: string,
  instance: string,
): EffectiveThreshold | undefined {
  const primary = primaryMetric(sensor).metric;
  const own = (m: string) => ruleOf(thresholds, m, instance);
  const other = thresholds.find((t) => t.metric !== primary && sensorOf(sensors, t.metric) === sensor)?.metric;
  return own(metric) ?? own(primary) ?? (other ? own(other) : undefined);
}

/** The level a threshold comes from; a server's row for one instance of several is the sensor's own. */
export function thresholdLevel(rule: EffectiveThreshold, several: boolean): RuleLevel {
  if (rule.origin === "default") return "builtin";
  if (rule.origin === "site") return "site";
  return several && rule.instance !== null ? "sensor" : "server";
}

/** The level a sensor's interval and timeout come from. */
export function timingLevel(sensor: EffectiveSensor): RuleLevel {
  return sensor.origin === "default" ? "builtin" : sensor.origin;
}

/** The most specific level of the rule: the badge of a sensor row. */
export function ruleLevel(sensor: EffectiveSensor, rule: EffectiveThreshold | undefined, several: boolean): RuleLevel {
  const timing = timingLevel(sensor);
  const alert = rule ? thresholdLevel(rule, several) : "builtin";
  return RANK[alert] > RANK[timing] ? alert : timing;
}

const SIGNS = { ">": ">", ">=": "≥", "<": "<", "<=": "≤" } as const;

function value(shape: Shape, v: number): string {
  if (shape.type === "percent") return formatPercent(v);
  return shape.type === "number" && shape.unit ? `${formatNumber(v)} ${shape.unit}` : formatNumber(v);
}

/** "every 1 min · warn > 70% · crit > 80% · for 5 min", or "every 1 min · critical when it fails". */
export function ruleSummary(shape: Shape, rule: EffectiveThreshold | undefined, intervalMs: number): string {
  const parts = [every(intervalMs)];
  if (!rule) {
    parts.push(t("sensors.summary.noAlert"));
  } else if (!rule.enabled) {
    parts.push(t("sensors.summary.off"));
  } else if (shape.type === "event") {
    parts.push(t(rule.critical !== null ? "sensors.summary.event.critical" : "sensors.summary.event.warning", { brief: shape.brief }));
  } else {
    const sign = SIGNS[rule.comparison];
    if (rule.warning !== null) parts.push(t("sensors.summary.warn", { sign, value: value(shape, rule.warning) }));
    if (rule.critical !== null) parts.push(t("sensors.summary.crit", { sign, value: value(shape, rule.critical) }));
  }
  if (rule?.enabled && rule.duration_s > 0) parts.push(t("sensors.summary.for", { duration: formatSeconds(rule.duration_s) }));
  return parts.join(" · ");
}

/**
 * The same rule for a table cell: "1 min · > 70% / 80% · 5 min" (interval,
 * warning / critical, how long), with `ruleSummary` as its tooltip.
 */
export function ruleCompact(shape: Shape, rule: EffectiveThreshold | undefined, intervalMs: number): string {
  const parts = [formatSeconds(intervalMs / 1000)];
  if (!rule) {
    parts.push(t("sensors.summary.noAlert"));
  } else if (!rule.enabled) {
    parts.push(t("sensors.summary.off"));
  } else if (shape.type === "event") {
    parts.push(t(rule.critical !== null ? "sensors.summary.event.critical" : "sensors.summary.event.warning", { brief: shape.brief }));
  } else {
    const limits = [rule.warning, rule.critical].filter((v): v is number => v !== null).map((v) => value(shape, v));
    if (limits.length > 0) parts.push(`${SIGNS[rule.comparison]} ${limits.join(" / ")}`);
  }
  if (rule?.enabled && rule.duration_s > 0) parts.push(formatSeconds(rule.duration_s));
  return parts.join(" · ");
}

/** Where a change applies: one instance of this server, this server, or every server of its site. */
export type ApplyTo = "sensor" | "server" | "site";

/** The choice preselected for an edit: the level in effect; a built-in rule is changed for this server. */
export function applyToFor(level: RuleLevel, several: boolean): ApplyTo {
  if (level === "sensor") return several ? "sensor" : "server";
  return level === "site" ? "site" : "server";
}

/** "disk", "service": what the sensor measures several of, for "every disk on web-1". */
export function instanceNoun(sensor: EffectiveSensor): string {
  switch (sensor.kind) {
    case "disk":
    case "diskio":
      return t("sensors.noun.disk");
    case "cpu":
      return t("sensors.noun.cpu");
    case "systemd":
    case "windows_service":
      return t("sensors.noun.service");
    default:
      return sensor.name;
  }
}

/** The "Apply to" choices of a sensor's edit, in words. */
export function applyToChoices(
  sensor: EffectiveSensor,
  instance: string,
  hostname: string,
  siteName: string,
): { value: ApplyTo; label: string }[] {
  const several = perInstance(sensor, instance);
  return [
    ...(several ? [{ value: "sensor" as const, label: t("sensors.apply.sensor", { instance }) }] : []),
    {
      value: "server",
      label: several
        ? t("sensors.apply.every", { noun: instanceNoun(sensor), host: hostname })
        : t("sensors.apply.server", { host: hostname }),
    },
    { value: "site", label: t("sensors.apply.site", { site: siteName }) },
  ];
}

/** What an edit saves: the sensor's timing, its threshold, or both; what did not change is left out. */
export interface RuleChange {
  timing?: { interval_ms: number; timeout_ms: number | null };
  threshold?: ThresholdRequest;
}

export interface RulePlace {
  serverId: string;
  siteId: string;
  sensor: EffectiveSensor;
  /** The instance the page shows; "" for none. */
  instance: string;
}

const ownerOf = (applyTo: ApplyTo, place: RulePlace): ConfigOwner =>
  applyTo === "site" ? { kind: "sites", id: place.siteId } : { kind: "servers", id: place.serverId };

/**
 * The instance a threshold row gets: the page's for this sensor only;
 * every instance of a sensor with several otherwise; else the rule's own,
 * like CPU usage's `total`.
 */
export function thresholdInstance(applyTo: ApplyTo, place: RulePlace, current: string | null): string | null {
  if (applyTo === "sensor") return place.instance;
  return perInstance(place.sensor, place.instance) ? null : current;
}

/**
 * Saves `change` where `applyTo` says (ADR-0016, ADR-0017): the timing as
 * the owner's sensor row, changed or copied from the inherited sensor,
 * since one sensor measures every instance; the threshold as the owner's
 * row for its metric and instance.
 */
export async function saveRule(place: RulePlace, applyTo: ApplyTo, change: RuleChange): Promise<void> {
  const owner = ownerOf(applyTo, place);
  if (change.timing) {
    const { entries } = await api.sensorRows(owner);
    const { sensor } = place;
    const row = entries.find((r) => r.enabled && r.name === sensor.name && r.instance === sensor.instance);
    const { kind, name, instance, params } = row ?? sensor;
    const body = { kind, name, instance, params, enabled: true, ...change.timing };
    await (row ? api.updateSensor(row.id, body) : api.createSensor(owner, body));
  }
  if (change.threshold) {
    await saveThreshold(owner, { ...change.threshold, instance: thresholdInstance(applyTo, place, change.threshold.instance) });
  }
}

/**
 * The server's own rows that make the rule differ from what it inherits:
 * a threshold for this instance, else one for the server, and its copy of
 * an inherited sensor (not a check added on the server, which removing
 * would delete). Empty when the server has none.
 */
export function ownOverrides(
  place: RulePlace,
  rule: EffectiveThreshold | undefined,
  rows: SensorConfigEntry[],
  siteSensors: EffectiveSensor[],
): { thresholdId: string | null; sensorRowId: string | null; level: RuleLevel | null } {
  const several = perInstance(place.sensor, place.instance);
  const { sensor } = place;
  // A check added on this server inherits nothing: its own alert is not an override.
  const inherited = siteSensors.some((s) => s.name === sensor.name && s.instance === sensor.instance);
  const ruleLevelNow = rule && rule.threshold_id ? thresholdLevel(rule, several) : null;
  const thresholdId =
    ruleLevelNow === "sensor" || (ruleLevelNow === "server" && inherited) ? rule!.threshold_id : null;
  const row = rows.find((r) => r.enabled && r.name === sensor.name && r.instance === sensor.instance);
  // A sensor-level threshold resets first; the server's timing waits for the next reset.
  const sensorRowId = ruleLevelNow === "sensor" || !inherited ? null : (row?.id ?? null);
  const level = ruleLevelNow === "sensor" ? "sensor" : thresholdId || sensorRowId ? "server" : null;
  return { thresholdId, sensorRowId, level };
}

/** Removes what `ownOverrides` found, going back to the inherited rule. */
export async function resetRule(overrides: { thresholdId: string | null; sensorRowId: string | null }): Promise<void> {
  if (overrides.thresholdId) await api.deleteThreshold(overrides.thresholdId);
  if (overrides.sensorRowId) await api.deleteSensor(overrides.sensorRowId);
}
