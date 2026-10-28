import {
  type Comparison,
  type ConfigOwner,
  type EffectiveSensor,
  type EffectiveThreshold,
  type SensorConfigRequest,
  type ThresholdRequest,
  api,
} from "./api";
import { formatNumber, formatSeconds } from "./format";
import { METRIC_PREFIX, metricTitle, shortMetric } from "./metrics";
import { sensorOf } from "./sensors";
import { SEVERITY_LABELS, type Severity } from "./thresholds";

/**
 * How a threshold reads and is edited (ADR-0057): a percentage or a plain
 * number with warning and critical levels, or an event (a port closes, a
 * log gets a matching line) that has one fixed trigger and a severity.
 */
export type Shape =
  | { type: "percent" | "number"; noun: string; suffix: string }
  | { type: "event"; noun: string; text: string; comparison: Comparison; value: number };

/** The fixed trigger of an event, by sensor kind and metric suffix. */
const EVENTS: Record<string, { suffix: string; text: string; comparison: Comparison; value: number }> = {
  port: { suffix: "up", text: "is closed", comparison: "<", value: 1 },
  systemd: { suffix: "active", text: "is not running", comparison: "<", value: 1 },
  windows_service: { suffix: "active", text: "is not running", comparison: "<", value: 1 },
  log: { suffix: "matches", text: "gets a matching line", comparison: ">", value: 0 },
  eventlog: { suffix: "matches", text: "gets an error", comparison: ">", value: 0 },
  script: { suffix: "status", text: "reports a problem", comparison: ">=", value: 1 },
};

/** "port 443", "service nginx", "/var/log/syslog": what a check watches, in words. */
function checkNoun(sensor: EffectiveSensor): string {
  const { params } = sensor;
  switch (sensor.kind) {
    case "port":
      return `port ${params.port ?? "?"}${params.host && params.host !== "127.0.0.1" ? ` on ${params.host}` : ""}`;
    case "systemd":
      return `service ${params.units ?? sensor.name}`;
    case "windows_service":
      return `service ${params.services ?? sensor.name}`;
    case "log":
      return params.path ?? sensor.name;
    case "eventlog":
      return `the ${params.channel ?? ""} event log`;
    case "script":
      return `script ${params.script ?? sensor.name}`;
    default:
      return sensor.name;
  }
}

/** The shape of the threshold on `metric` and `instance` (null: every instance). */
export function shapeOf(sensors: EffectiveSensor[], metric: string, instance: string | null): Shape {
  const short = shortMetric(metric);
  if (short === "cpu_usage_ratio") return { type: "percent", noun: "CPU usage", suffix: "" };
  if (short === "memory_available_ratio") return { type: "percent", noun: "available memory", suffix: "" };
  if (short === "disk_used_ratio") return { type: "percent", noun: instance ? `disk ${instance}` : "a disk", suffix: " full" };
  const sensor = sensorOf(sensors, metric);
  const event = sensor && EVENTS[sensor.kind];
  if (sensor && event && short === `${sensor.name}_${event.suffix}`) {
    const { text, comparison, value } = event;
    return { type: "event", noun: checkNoun(sensor), text, comparison, value };
  }
  const noun = `${metricTitle(metric)}${instance ? ` ${instance}` : ""}`;
  return { type: short.endsWith("_ratio") ? "percent" : "number", noun, suffix: "" };
}

/** An event threshold whose values are not the event's own reads as a number. */
export function shapeFor(sensors: EffectiveSensor[], rule: EffectiveThreshold): Shape {
  const shape = shapeOf(sensors, rule.metric, rule.instance);
  if (shape.type !== "event") return shape;
  const values = [rule.warning, rule.critical].filter((v) => v !== null);
  const own = rule.comparison === shape.comparison && values.length === 1 && values[0] === shape.value;
  return own || !rule.enabled ? shape : { type: "number", noun: shape.noun, suffix: "" };
}

const WORDS: Record<Comparison, string> = { ">": "over", ">=": "at or over", "<": "under", "<=": "at or under" };

const show = (shape: Shape, value: number) =>
  shape.type === "percent" ? `${formatNumber(value * 100, 1)}%` : formatNumber(value);

/** "Warn when a disk is over 85% full, critical over 95%, for 5 min." */
export function describeThreshold(shape: Shape, rule: EffectiveThreshold): string {
  if (!rule.enabled) return `No alert on ${shape.noun}.`;
  const duration = rule.duration_s > 0 ? ` for ${formatSeconds(rule.duration_s)}` : "";
  if (shape.type === "event") {
    const severity = rule.critical !== null ? "Critical" : "Warning";
    return `${severity} when ${shape.noun} ${shape.text}${duration}.`;
  }
  const word = WORDS[rule.comparison];
  const parts: string[] = [];
  if (rule.warning !== null) parts.push(`Warn when ${shape.noun} is ${word} ${show(shape, rule.warning)}${shape.suffix}`);
  if (rule.critical !== null) {
    const level = `${word} ${show(shape, rule.critical)}`;
    parts.push(parts.length ? `critical ${level}` : `Critical when ${shape.noun} is ${level}${shape.suffix}`);
  }
  return `${parts.join(", ")}${parts.length > 1 && duration ? "," : ""}${duration}.`;
}

/** What the threshold form holds, as typed. */
export interface ThresholdForm {
  warning: string;
  critical: string;
  severity: Severity;
  minutes: string;
}

/** The form prefilled with `rule`. */
export function formOf(shape: Shape, rule: EffectiveThreshold): ThresholdForm {
  const field = (value: number | null) =>
    value === null ? "" : shape.type === "percent" ? formatNumber(value * 100, 1) : String(value);
  return {
    warning: field(rule.warning),
    critical: field(rule.critical),
    severity: rule.critical !== null || rule.warning === null ? "critical" : "warning",
    minutes: String(Math.round((rule.duration_s / 60) * 10) / 10),
  };
}

export type FormErrors = Partial<Record<"warning" | "critical" | "minutes", string>>;

/** The request for `form`, or what is wrong with it, field by field. */
export function thresholdRequest(
  shape: Shape,
  rule: EffectiveThreshold,
  form: ThresholdForm,
): { request: ThresholdRequest } | { errors: FormErrors } {
  const errors: FormErrors = {};
  const minutes = Number(form.minutes.trim() || "0");
  if (!Number.isFinite(minutes) || minutes < 0 || minutes > 1440) {
    errors.minutes = "Enter a number of minutes from 0 to 1440.";
  }
  const base = { metric: rule.metric, instance: rule.instance, hysteresis: rule.hysteresis, enabled: true };
  const duration_s = Math.round(minutes * 60);
  if (shape.type === "event") {
    if (errors.minutes) return { errors };
    const set = (severity: Severity) => (form.severity === severity ? shape.value : null);
    return {
      request: { ...base, comparison: shape.comparison, warning: set("warning"), critical: set("critical"), duration_s },
    };
  }
  const parse = (key: "warning" | "critical"): number | null => {
    const text = form[key].trim().replace(/%$/, "").trim();
    if (text === "") return null;
    const value = Number(text);
    if (!Number.isFinite(value) || (shape.type === "percent" && (value < 0 || value > 100))) {
      errors[key] = shape.type === "percent" ? "Enter a percentage from 0 to 100, or leave it empty." : "Enter a number, or leave it empty.";
      return null;
    }
    return shape.type === "percent" ? value / 100 : value;
  };
  const warning = parse("warning");
  const critical = parse("critical");
  if (!errors.warning && !errors.critical) {
    const above = rule.comparison === ">" || rule.comparison === ">=";
    if (warning === null && critical === null) {
      errors.warning = "Set a warning level, a critical level, or both.";
    } else if (warning !== null && critical !== null && (above ? critical < warning : critical > warning)) {
      errors.critical = `The critical level must be ${above ? "at or above" : "at or below"} the warning level.`;
    }
  }
  if (Object.keys(errors).length > 0) return { errors };
  return { request: { ...base, comparison: rule.comparison, warning, critical, duration_s } };
}

/**
 * Writes `request` as `owner`'s own threshold for its metric and instance:
 * replaces the row it has, or adds one. Inherited values stay untouched.
 */
export async function saveThreshold(owner: ConfigOwner, request: ThresholdRequest): Promise<void> {
  const { entries } = await api.thresholdRows(owner);
  const row = entries.find((e) => e.metric === request.metric && e.instance === request.instance);
  await (row ? api.updateThreshold(row.id, request) : api.createThreshold(owner, request));
}

/** One field of the Add a check form. */
export interface CheckField {
  key: string;
  label: string;
  placeholder: string;
  /** Prefilled value; the field may be left as is. */
  initial?: string;
  hint?: string;
  /** Why `value` is refused, if it is; the API checks the rest. */
  check?: (value: string) => string | null;
}

/** A kind of check one can add, with the alert that comes with it. */
export interface CheckType {
  kind: string;
  label: string;
  /** Which servers run it: `null` for every one. */
  os: "linux" | "windows" | null;
  fields: CheckField[];
  /** Parameters the form does not ask for. */
  fixed?: Record<string, string>;
  /** The field the check's name is made from. */
  nameFrom: string;
  severity: Severity;
  duration_s: number;
}

const portNumber = (value: string) =>
  /^\d+$/.test(value) && Number(value) >= 1 && Number(value) <= 65_535 ? null : "Enter a port from 1 to 65535.";
const oneName = (value: string) => (/^[^,\s]+$/.test(value) ? null : "Enter one name, without spaces or commas.");

export const CHECK_TYPES: CheckType[] = [
  {
    kind: "port",
    label: "A port answers",
    os: null,
    fields: [
      { key: "port", label: "Port", placeholder: "443", check: portNumber },
      { key: "host", label: "Host", placeholder: "127.0.0.1", initial: "127.0.0.1", hint: "Where the agent connects: this server by default." },
    ],
    nameFrom: "port",
    severity: "critical",
    duration_s: 60,
  },
  {
    kind: "systemd",
    label: "A service is running",
    os: "linux",
    fields: [{ key: "units", label: "Service", placeholder: "nginx", hint: "The systemd unit name.", check: oneName }],
    nameFrom: "units",
    severity: "critical",
    duration_s: 60,
  },
  {
    kind: "windows_service",
    label: "A Windows service is running",
    os: "windows",
    fields: [{ key: "services", label: "Service", placeholder: "W32Time", hint: "The service name, not its display name.", check: oneName }],
    nameFrom: "services",
    severity: "critical",
    duration_s: 60,
  },
  {
    kind: "log",
    label: "A log file gets a line",
    os: "linux",
    fields: [
      {
        key: "path",
        label: "Log file",
        placeholder: "/var/log/syslog",
        hint: "Under /var/log unless the agent allows more.",
        check: (v) => (v.startsWith("/") ? null : "Enter a full path, starting with /."),
      },
      { key: "pattern", label: "Lines containing", placeholder: "error|failed", hint: "Text or a regular expression." },
    ],
    nameFrom: "path",
    severity: "warning",
    duration_s: 0,
  },
  {
    kind: "eventlog",
    label: "Windows logs an error",
    os: "windows",
    fields: [{ key: "channel", label: "Event log", placeholder: "System", initial: "System", hint: "System or Application." }],
    fixed: { levels: "critical,error" },
    nameFrom: "channel",
    severity: "warning",
    duration_s: 0,
  },
  {
    kind: "script",
    label: "A script reports OK",
    os: null,
    fields: [
      {
        key: "script",
        label: "Script",
        placeholder: "check_backup.sh",
        hint: "A file an administrator installed in the agent's scripts folder.",
        check: (v) => (/[/\\]/.test(v) ? "Enter the file name only." : null),
      },
    ],
    nameFrom: "script",
    severity: "warning",
    duration_s: 60,
  },
];

/** Which checks a server can run; every check for a site, which can mix systems. */
export function checkTypesFor(osFamily: string | null): CheckType[] {
  if (osFamily === null) return CHECK_TYPES;
  const os = osFamily === "windows" ? "windows" : "linux";
  return CHECK_TYPES.filter((t) => t.os === null || t.os === os);
}

/** Longest sensor name the agent accepts (ADR-0012). */
const MAX_NAME = 64;

/** "service_nginx": a sensor name for the check, which prefixes its metrics. */
export function checkName(type: CheckType, values: Record<string, string>): string {
  const prefix = type.kind === "systemd" || type.kind === "windows_service" ? "service" : type.kind;
  const source = (values[type.nameFrom] ?? "").split("/").pop() ?? "";
  const slug = source
    .toLowerCase()
    .replace(/\.(service|log|sh|ps1)$/, "")
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return `${prefix}${slug ? `_${slug}` : ""}`.slice(0, MAX_NAME).replace(/_+$/, "");
}

/** What is wrong with each field, or nothing. */
export function checkErrors(type: CheckType, values: Record<string, string>): Record<string, string> {
  const errors: Record<string, string> = {};
  for (const field of type.fields) {
    const value = (values[field.key] ?? "").trim();
    const error = value === "" ? `Enter the ${field.label.toLowerCase()}.` : (field.check?.(value) ?? null);
    if (error) errors[field.key] = error;
  }
  return errors;
}

/** Interval of an added check: the built-in sensors' one. */
export const CHECK_INTERVAL_MS = 60_000;

/** The sensor row and the threshold that alerts on it. */
export function checkRequests(
  type: CheckType,
  values: Record<string, string>,
): { sensor: SensorConfigRequest; threshold: ThresholdRequest } {
  const name = checkName(type, values);
  const params = Object.fromEntries(type.fields.map((f) => [f.key, (values[f.key] ?? "").trim()]));
  const event = EVENTS[type.kind]!;
  const level = (severity: Severity) => (type.severity === severity ? event.value : null);
  return {
    sensor: { kind: type.kind, name, interval_ms: CHECK_INTERVAL_MS, params: { ...params, ...type.fixed } },
    threshold: {
      metric: `${METRIC_PREFIX}${name}_${event.suffix}`,
      instance: null,
      comparison: event.comparison,
      warning: level("warning"),
      critical: level("critical"),
      duration_s: type.duration_s,
      hysteresis: 0,
      enabled: true,
    },
  };
}

/** "Port 443", "Service nginx", "Disk space": a check in the list. */
export function checkTitle(sensor: EffectiveSensor): string {
  switch (sensor.kind) {
    case "cpu":
      return "CPU usage";
    case "memory":
      return "Memory";
    case "disk":
      return `Disk space${sensor.instance ? ` ${sensor.instance}` : ""}`;
    case "log":
      return `“${sensor.params.pattern ?? ""}” in ${sensor.params.path ?? sensor.name}`;
    case "eventlog":
      return `Errors in the ${sensor.params.channel ?? ""} event log`;
    default: {
      const noun = checkNoun(sensor);
      return noun.charAt(0).toUpperCase() + noun.slice(1);
    }
  }
}

/** "Warning" or "Critical", for the severity select. */
export const SEVERITIES = (["warning", "critical"] as const).map((value) => ({ value, label: SEVERITY_LABELS[value] }));
