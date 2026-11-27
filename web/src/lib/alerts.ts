import { type PlainKey, t } from "@/i18n";

import {
  type Comparison,
  type ConfigOwner,
  type EffectiveSensor,
  type EffectiveThreshold,
  type PluginSetting,
  type SensorConfigEntry,
  type SensorConfigRequest,
  type ThresholdRequest,
  type UsablePlugin,
  api,
} from "./api";
import { formatNumber, formatPercent, formatSeconds } from "./format";
import { METRIC_PREFIX, metricTitle, shortMetric } from "./metrics";
import { sensorOf } from "./sensors";
import { websiteHost, websiteLabel } from "./website";
import { SEVERITY_LABELS, type Severity } from "./thresholds";

/**
 * How a threshold reads and is edited (ADR-0057): a percentage or a plain
 * number with warning and critical levels, or an event (a port closes, a
 * log gets a matching line) that has one fixed trigger and a severity.
 */
export type Shape =
  | { type: "percent" | "number"; noun: string; suffix: string; unit?: string }
  | { type: "event"; noun: string; text: string; brief: string; comparison: Comparison; value: number };

type EventWords = "port" | "ping" | "snmp" | "service" | "log" | "eventlog" | "script" | "website" | "certificate";

/**
 * The fixed trigger of an event, by sensor kind and metric suffix; its
 * words' `brief` follows the severity in a rule summary: "critical when
 * it fails".
 */
const EVENTS: Record<string, { suffix: string; words: EventWords; comparison: Comparison; value: number }> = {
  port: { suffix: "up", words: "port", comparison: "<", value: 1 },
  ping: { suffix: "up", words: "ping", comparison: "<", value: 1 },
  snmp: { suffix: "up", words: "snmp", comparison: "<", value: 1 },
  systemd: { suffix: "active", words: "service", comparison: "<", value: 1 },
  windows_service: { suffix: "active", words: "service", comparison: "<", value: 1 },
  log: { suffix: "matches", words: "log", comparison: ">", value: 0 },
  eventlog: { suffix: "matches", words: "eventlog", comparison: ">", value: 0 },
  script: { suffix: "status", words: "script", comparison: ">=", value: 1 },
  http: { suffix: "up", words: "website", comparison: "<", value: 1 },
};

/** Other events of a kind than its own, like a website's certificate (ADR-0150). */
const MORE_EVENTS: Record<string, { suffix: string; words: EventWords; comparison: Comparison; value: number }[]> = {
  http: [{ suffix: "cert_ok", words: "certificate", comparison: "<", value: 1 }],
};


/** A port check's host, when it is not this server. */
const remoteHost = (params: Record<string, string>) => (params.host && params.host !== "127.0.0.1" ? params.host : null);

/** "port 443", "service nginx", "/var/log/syslog": what a check watches, in words. */
function checkNoun(sensor: EffectiveSensor): string {
  const { params } = sensor;
  switch (sensor.kind) {
    case "port": {
      const host = remoteHost(params);
      const port = params.port ?? "?";
      if (params.protocol === "udp") return t("checks.noun.udpPort", { port });
      return host ? t("checks.noun.portOn", { port, host }) : t("checks.noun.port", { port });
    }
    case "ping":
      return params.host ? t("checks.noun.ping", { host: params.host }) : t("checks.noun.pingDevice");
    case "systemd":
      return t("checks.noun.service", { name: params.units ?? sensor.name });
    case "windows_service":
      return t("checks.noun.service", { name: params.services ?? sensor.name });
    case "log":
      return params.path ?? sensor.name;
    case "eventlog":
      return t("checks.noun.eventlog", { channel: params.channel ?? "" });
    case "script":
      return t("checks.noun.script", { name: params.script ?? sensor.name });
    case "http":
      return t("checks.noun.website", { host: websiteLabel(params.url) || sensor.name });
    default:
      return sensor.name;
  }
}

/** The shape of the threshold on `metric` and `instance` (null: every instance). */
export function shapeOf(sensors: EffectiveSensor[], metric: string, instance: string | null): Shape {
  const short = shortMetric(metric);
  if (short === "cpu_usage_ratio") return { type: "percent", noun: t("checks.noun.cpu"), suffix: "" };
  if (short === "memory_available_ratio") return { type: "percent", noun: t("checks.noun.memory"), suffix: "" };
  if (short === "disk_used_ratio") {
    return { type: "percent", noun: instance ? t("checks.noun.disk", { instance }) : t("checks.noun.aDisk"), suffix: t("checks.suffix.full") };
  }
  if (short === "diskio_wait_seconds") {
    const noun = instance ? t("checks.noun.wait", { instance }) : t("checks.noun.waitAny");
    return { type: "number", noun, suffix: "", unit: t("format.unit.second") };
  }
  const sensor = sensorOf(sensors, metric);
  const event =
    sensor && [EVENTS[sensor.kind], ...(MORE_EVENTS[sensor.kind] ?? [])].find((e) => e && short === `${sensor.name}_${e.suffix}`);
  if (sensor?.kind === "http") {
    const host = websiteHost(sensor.params.url) || sensor.name;
    if (short === `${sensor.name}_cert_days_left`) {
      return { type: "number", noun: t("checks.noun.certificateDays", { host }), suffix: "" };
    }
    if (short === `${sensor.name}_response_seconds`) {
      return { type: "number", noun: t("checks.noun.responseTime", { host: websiteLabel(sensor.params.url) || sensor.name }), suffix: "", unit: t("format.unit.second") };
    }
  }
  if (sensor && event) {
    const { words, comparison, value } = event;
    const text = t(`checks.event.${words}.text`);
    const brief = t(`checks.event.${words}.brief`);
    const noun =
      words === "certificate" ? t("checks.noun.certificate", { host: websiteHost(sensor.params.url) || sensor.name }) : checkNoun(sensor);
    return { type: "event", noun, text, brief, comparison, value };
  }
  const noun = `${metricTitle(metric)}${instance ? ` ${instance}` : ""}`;
  if (short.endsWith("_ratio")) return { type: "percent", noun, suffix: "" };
  return { type: "number", noun, suffix: "", ...(short.endsWith("_seconds") && { unit: t("format.unit.second") }) };
}

/** An event threshold whose values are not the event's own reads as a number. */
export function shapeFor(sensors: EffectiveSensor[], rule: EffectiveThreshold): Shape {
  const shape = shapeOf(sensors, rule.metric, rule.instance);
  if (shape.type !== "event") return shape;
  const values = [rule.warning, rule.critical].filter((v) => v !== null);
  const own = rule.comparison === shape.comparison && values.length === 1 && values[0] === shape.value;
  return own || !rule.enabled ? shape : { type: "number", noun: shape.noun, suffix: "" };
}

const show = (shape: Shape, value: number) => {
  if (shape.type === "percent") return formatPercent(value);
  return shape.type === "number" && shape.unit ? `${formatNumber(value)} ${shape.unit}` : formatNumber(value);
};

/** "Warn when a disk is over 85% full, critical over 95%, for 5 min." */
export function describeThreshold(shape: Shape, rule: EffectiveThreshold): string {
  const { noun } = shape;
  if (!rule.enabled) return t("checks.describe.none", { noun });
  const duration = rule.duration_s > 0 ? t("checks.describe.duration", { duration: formatSeconds(rule.duration_s) }) : "";
  if (shape.type === "event") {
    const key = rule.critical !== null ? "checks.describe.event.critical" : "checks.describe.event.warning";
    return t(key, { noun, text: shape.text, duration });
  }
  const { suffix } = shape;
  const word = t(`checks.word.${rule.comparison}`);
  const parts: string[] = [];
  if (rule.warning !== null) parts.push(t("checks.describe.warn", { noun, word, value: show(shape, rule.warning), suffix }));
  if (rule.critical !== null) {
    const value = show(shape, rule.critical);
    parts.push(parts.length ? t("checks.describe.critAlso", { word, value }) : t("checks.describe.crit", { noun, word, value, suffix }));
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
  // Plain digits, which `thresholdRequest` reads back in any language.
  const field = (value: number | null) =>
    value === null ? "" : shape.type === "percent" ? String(Math.round(value * 1000) / 10) : String(value);
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
  const minutes = Number(form.minutes.trim().replace(",", ".") || "0");
  if (!Number.isFinite(minutes) || minutes < 0 || minutes > 1440) {
    errors.minutes = t("checks.error.minutes");
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
    // A French decimal comma reads as a point.
    const text = form[key].trim().replace(/%$/, "").trim().replace(",", ".");
    if (text === "") return null;
    const value = Number(text);
    if (!Number.isFinite(value) || (shape.type === "percent" && (value < 0 || value > 100))) {
      errors[key] = shape.type === "percent" ? t("checks.error.percent") : t("checks.error.number");
      return null;
    }
    return shape.type === "percent" ? value / 100 : value;
  };
  const warning = parse("warning");
  const critical = parse("critical");
  if (!errors.warning && !errors.critical) {
    const above = rule.comparison === ">" || rule.comparison === ">=";
    if (warning === null && critical === null) {
      errors.warning = t("checks.error.levels");
    } else if (warning !== null && critical !== null && (above ? critical < warning : critical > warning)) {
      errors.critical = above ? t("checks.error.criticalAbove") : t("checks.error.criticalBelow");
    }
  }
  if (Object.keys(errors).length > 0) return { errors };
  return { request: { ...base, comparison: rule.comparison, warning, critical, duration_s } };
}

/**
 * Writes `request` as `owner`'s own threshold for its metric, instance and kind:
 * replaces the row it has, or adds one. Inherited values stay untouched.
 */
export async function saveThreshold(owner: ConfigOwner, request: ThresholdRequest): Promise<void> {
  const { entries } = await api.thresholdRows(owner);
  // One row per metric, instance and kind (ADR-0153).
  const kind = request.kind ?? "value";
  const row = entries.find((e) => e.metric === request.metric && e.instance === request.instance && (e.kind ?? "value") === kind);
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
  /** May be left empty. */
  optional?: boolean;
  /** Shown under "More options". */
  advanced?: boolean;
  /** How it is typed: a line of text by default. */
  input?: "text" | "password" | "lines" | "select";
  /** The choices of a `select`, its value first. */
  options?: { value: string; label: string }[];
}

/** A kind of check one can add, with the alert that comes with it. */
export interface CheckType {
  kind: string;
  label: string;
  /** Which servers run it: `null` for every one. */
  os: "linux" | "windows" | null;
  fields: CheckField[];
  /** Field texts that differ on one system, by field key; a site shows the plain ones. */
  on?: Partial<Record<"linux" | "windows", Record<string, Partial<CheckField>>>>;
  /** Parameters the form does not ask for. */
  fixed?: Record<string, string>;
  /** The field the check's name is made from. */
  nameFrom: string;
  /** What the check's name starts with, when not its kind. */
  prefix?: string;
  severity: Severity;
  duration_s: number;
  /** No alert comes with it: a plugin's metrics are its own (ADR-0103). */
  noAlert?: boolean;
  /** Runs from the central server or a relay, which the form asks (ADR-0148). */
  probe?: boolean;
  /** Runs on the agent unless the form names a probe (ADR-0150). */
  anyRunner?: boolean;
  /** Its alerts are its template's, added by the server (ADR-0183). */
  template?: boolean;
  /** Alerts that come with it besides its own event, like a website's certificate (ADR-0150). */
  moreAlerts?: (metricOf: (suffix: string) => string, values: Record<string, string>) => ThresholdRequest[];
}

const portNumber = (value: string) =>
  /^\d+$/.test(value) && Number(value) >= 1 && Number(value) <= 65_535 ? null : t("checks.error.port");
const oneName = (value: string) => (/^[^,\s]+$/.test(value) ? null : t("checks.error.oneName"));
const websiteUrl = (value: string) => {
  try {
    const url = new URL(value);
    if (url.protocol !== "http:" && url.protocol !== "https:") return t("checks.error.websiteUrl");
    return url.username || url.password ? t("checks.error.urlCredentials") : null;
  } catch {
    return t("checks.error.websiteUrl");
  }
};
const statusCodes = (value: string) =>
  /^\s*[1-5]\d\d(\s*-\s*[1-5]\d\d)?(\s*,\s*[1-5]\d\d(\s*-\s*[1-5]\d\d)?)*\s*$/.test(value) ? null : t("checks.error.status");

/**
 * The kinds of check, with their words in the active language: labels,
 * fields and per-system texts are read when shown, so the list itself is
 * built once.
 */
export const CHECK_TYPES: CheckType[] = [
  {
    kind: "port",
    get label() {
      return t("checks.type.port");
    },
    os: null,
    get fields() {
      return [
        { key: "port", label: t("checks.field.port"), placeholder: "443", check: portNumber },
        {
          key: "host",
          label: t("checks.field.address"),
          placeholder: "127.0.0.1",
          initial: "127.0.0.1",
          hint: t("checks.field.addressHint"),
        },
      ];
    },
    nameFrom: "port",
    severity: "critical",
    duration_s: 60,
  },
  {
    kind: "systemd",
    get label() {
      return t("checks.type.systemd");
    },
    os: "linux",
    get fields() {
      return [{ key: "units", label: t("checks.field.service"), placeholder: "nginx", hint: t("checks.field.unitHint"), check: oneName }];
    },
    nameFrom: "units",
    severity: "critical",
    duration_s: 60,
  },
  {
    kind: "windows_service",
    get label() {
      return t("checks.type.windowsService");
    },
    os: "windows",
    get fields() {
      return [
        {
          key: "services",
          label: t("checks.field.service"),
          placeholder: "W32Time",
          hint: t("checks.field.windowsServiceHint"),
          check: oneName,
        },
      ];
    },
    nameFrom: "services",
    severity: "critical",
    duration_s: 60,
  },
  {
    kind: "log",
    get label() {
      return t("checks.type.log");
    },
    os: null,
    get fields() {
      return [
        {
          key: "path",
          label: t("checks.field.logFile"),
          placeholder: "/var/log/syslog",
          hint: t("checks.field.logFileHint"),
          check: (v: string) => (/^(\/|[A-Za-z]:\\)/.test(v) ? null : t("checks.error.path")),
        },
        { key: "pattern", label: t("checks.field.lines"), placeholder: "error|failed", hint: t("checks.field.linesHint") },
      ];
    },
    get on() {
      return {
        linux: { path: { hint: t("checks.field.logFileLinuxHint") } },
        windows: { path: { placeholder: "C:\\Logs\\app.log", hint: t("checks.field.logFileWindowsHint") } },
      };
    },
    nameFrom: "path",
    severity: "warning",
    duration_s: 0,
  },
  {
    kind: "eventlog",
    get label() {
      return t("checks.type.eventlog");
    },
    os: "windows",
    get fields() {
      return [
        { key: "channel", label: t("checks.field.eventLog"), placeholder: "System", initial: "System", hint: t("checks.field.eventLogHint") },
      ];
    },
    fixed: { levels: "critical,error" },
    nameFrom: "channel",
    severity: "warning",
    duration_s: 0,
  },
  {
    kind: "script",
    get label() {
      return t("checks.type.script");
    },
    os: null,
    get fields() {
      return [
        {
          key: "script",
          label: t("checks.field.script"),
          placeholder: "check_backup.sh",
          hint: t("checks.field.scriptHint"),
          check: (v: string) => (/[/\\]/.test(v) ? t("checks.error.fileName") : null),
        },
      ];
    },
    nameFrom: "script",
    severity: "warning",
    duration_s: 60,
  },
  websiteType(false),
  ...appTypes(false),
];

const hostField = (): CheckField => ({
  key: "host",
  label: t("devices.check.host"),
  placeholder: "",
  hint: t("devices.check.hostHint"),
  optional: true,
});

/** The fields every database and Redis check asks for (ADR-0183). */
function connectionFields(port: number, user: string | null, device: boolean): CheckField[] {
  return [
    device
      ? hostField()
      : { key: "host", label: t("checks.field.address"), placeholder: "127.0.0.1", initial: "127.0.0.1", hint: t("checks.field.appAddressHint") },
    { key: "port", label: t("checks.field.port"), placeholder: String(port), initial: String(port), check: portNumber },
    {
      key: "user",
      label: t("checks.field.username"),
      placeholder: user ?? "",
      initial: user ?? undefined,
      hint: t("checks.field.appUserHint"),
      optional: user === null,
    },
    { key: "password", label: t("checks.field.password"), placeholder: "", hint: t("checks.field.appPasswordHint"), optional: true, input: "password" },
  ];
}

const tlsField = (): CheckField => ({
  key: "tls",
  label: t("checks.field.tls"),
  placeholder: "",
  initial: "prefer",
  optional: true,
  advanced: true,
  input: "select",
  options: [
    { value: "prefer", label: t("checks.field.tlsPrefer") },
    { value: "verify", label: t("checks.field.tlsVerify") },
    { value: "off", label: t("checks.field.tlsOff") },
  ],
});

/**
 * The ready-made application checks (ADR-0183): databases and Redis run by
 * the agent or, chosen in the form, a probe; a device's by a probe.
 * Docker is read by the server's agent only. Each comes with its
 * template's alerts, which the server adds.
 */
function appTypes(device: boolean): CheckType[] {
  const common = { os: null, nameFrom: "", severity: "critical" as const, duration_s: 120, template: true, probe: device, anyRunner: !device };
  const types: CheckType[] = [
    {
      ...common,
      kind: "postgresql",
      get label() {
        return t("checks.type.postgresql");
      },
      get fields(): CheckField[] {
        return [
          ...connectionFields(5432, "monitorflow", device),
          { key: "database", label: t("checks.field.database"), placeholder: "postgres", initial: "postgres", optional: true, advanced: true },
          tlsField(),
          { key: "slow_after", label: t("checks.field.slowAfter"), placeholder: "30", initial: "30", hint: t("checks.field.slowAfterHint"), optional: true, advanced: true },
        ];
      },
    },
    {
      ...common,
      kind: "mysql",
      get label() {
        return t("checks.type.mysql");
      },
      get fields(): CheckField[] {
        return [...connectionFields(3306, "monitorflow", device), tlsField()];
      },
    },
    {
      ...common,
      kind: "mssql",
      get label() {
        return t("checks.type.mssql");
      },
      get fields(): CheckField[] {
        const fields = connectionFields(1433, "monitorflow", device);
        fields[2] = { ...fields[2]!, hint: t("checks.field.mssqlUserHint") };
        return [...fields, tlsField()];
      },
    },
    {
      ...common,
      kind: "redis",
      get label() {
        return t("checks.type.redis");
      },
      get fields(): CheckField[] {
        return connectionFields(6379, null, device);
      },
    },
    {
      ...common,
      kind: "jmx",
      get label() {
        return t("checks.type.jmx");
      },
      get fields(): CheckField[] {
        const hints: Record<string, string> = {
          port: t("checks.field.jolokiaPortHint"),
          user: t("checks.field.jolokiaUserHint"),
          password: t("checks.field.jolokiaPasswordHint"),
        };
        return [
          ...connectionFields(8778, null, device).map((f) => ({ ...f, hint: hints[f.key] ?? f.hint })),
          {
            key: "groups",
            label: t("checks.field.jmxGroups"),
            placeholder: "",
            initial: "tomcat,kafka",
            optional: true,
            input: "select",
            options: [
              { value: "tomcat,kafka", label: t("checks.field.jmxGroupsBoth") },
              { value: "tomcat", label: "Tomcat" },
              { value: "kafka", label: "Kafka" },
              { value: "none", label: t("checks.field.jmxGroupsNone") },
            ],
          },
          { key: "path", label: t("checks.field.jolokiaPath"), placeholder: "/jolokia/", initial: "/jolokia/", optional: true, advanced: true },
          {
            key: "tls",
            label: t("checks.field.tls"),
            placeholder: "",
            initial: "off",
            optional: true,
            advanced: true,
            input: "select",
            options: [
              { value: "off", label: t("checks.field.tlsHttp") },
              { value: "verify", label: t("checks.field.tlsVerify") },
            ],
          },
          {
            key: "fingerprint",
            label: t("checks.field.fingerprint"),
            placeholder: "AB:CD:…",
            hint: t("checks.field.fingerprintHint"),
            optional: true,
            advanced: true,
            check: (v: string) => (/^[0-9a-f]{64}$/i.test(v.replace(/[:\s-]/g, "")) ? null : t("checks.error.fingerprint")),
          },
        ];
      },
    },
  ];
  // A cluster, read through its API from the central server or a relay.
  const kubernetes: CheckType = {
    ...common,
    kind: "kubernetes",
    get label() {
      return t("checks.type.kubernetes");
    },
    get fields(): CheckField[] {
      return [
        hostField(),
        { key: "port", label: t("checks.field.port"), placeholder: "6443", initial: "6443", check: portNumber },
        { key: "token", label: t("checks.field.token"), placeholder: "", hint: t("checks.field.tokenHint"), input: "password" },
        { key: "ca_certificate", label: t("checks.field.caCertificate"), placeholder: "LS0tLS1CRUdJTi…", hint: t("checks.field.caCertificateHint"), optional: true },
        { key: "namespaces", label: t("checks.field.namespaces"), placeholder: "^(shop|billing)$", hint: t("checks.field.namespacesHint"), optional: true, advanced: true },
      ];
    },
  };
  if (device) return [...types, kubernetes];
  return [
    ...types,
    {
      ...common,
      kind: "docker",
      os: "linux",
      probe: false,
      anyRunner: false,
      get label() {
        return t("checks.type.docker");
      },
      get fields(): CheckField[] {
        return [
          { key: "containers", label: t("checks.field.containers"), placeholder: "^web-", hint: t("checks.field.containersHint"), optional: true },
          {
            key: "engine",
            label: t("checks.field.engine"),
            placeholder: "",
            initial: "docker",
            optional: true,
            advanced: true,
            input: "select",
            options: [
              { value: "docker", label: "Docker" },
              { value: "podman", label: "Podman" },
            ],
          },
        ];
      },
    },
  ];
}

/** The names of the application checks that connect to an address. */
const APP_TITLES: Record<"postgresql" | "mysql" | "mssql" | "redis" | "jmx", PlainKey> = {
  postgresql: "checks.type.postgresql",
  mysql: "checks.type.mysql",
  mssql: "checks.type.mssql",
  redis: "checks.type.redis",
  jmx: "checks.type.jmx",
};

/** What a device can be checked with, from the central server or a relay (ADR-0148). */
/**
 * A website check (ADR-0150): run by the server's agent or, chosen in
 * the form, by a probe; a device's always by a probe (`probe`).
 */
function websiteType(probe: boolean): CheckType {
  return {
    kind: "http",
    get label() {
      return t("checks.type.http");
    },
    os: null,
    get fields(): CheckField[] {
      return [
        { key: "url", label: t("checks.field.url"), placeholder: "https://www.example.com/", hint: t("checks.field.urlHint"), check: websiteUrl },
        { key: "contains", label: t("checks.field.contains"), placeholder: t("checks.field.containsPlaceholder"), hint: t("checks.field.containsHint"), optional: true },
        { key: "not_contains", label: t("checks.field.notContains"), placeholder: t("checks.field.notContainsPlaceholder"), hint: t("checks.field.notContainsHint"), optional: true, advanced: true },
        { key: "status", label: t("checks.field.status"), placeholder: "200-299", hint: t("checks.field.statusHint"), optional: true, advanced: true, check: statusCodes },
        {
          key: "follow_redirects",
          label: t("checks.field.redirects"),
          placeholder: "",
          initial: "true",
          optional: true,
          advanced: true,
          input: "select",
          options: [
            { value: "true", label: t("checks.field.redirectsFollow") },
            { value: "false", label: t("checks.field.redirectsKeep") },
          ],
        },
        {
          key: "method",
          label: t("checks.field.method"),
          placeholder: "",
          initial: "GET",
          optional: true,
          advanced: true,
          input: "select",
          options: [
            { value: "GET", label: t("checks.field.methodGet") },
            { value: "HEAD", label: t("checks.field.methodHead") },
          ],
        },
        {
          key: "ip",
          label: t("checks.field.ip"),
          placeholder: "",
          initial: "any",
          optional: true,
          advanced: true,
          input: "select",
          options: [
            { value: "any", label: t("checks.field.ipAny") },
            { value: "4", label: "IPv4" },
            { value: "6", label: "IPv6" },
          ],
        },
        { key: "headers", label: t("checks.field.headers"), placeholder: "Accept-Language: en", hint: t("checks.field.headersHint"), optional: true, advanced: true, input: "lines" },
        { key: "username", label: t("checks.field.username"), placeholder: "", hint: t("checks.field.usernameHint"), optional: true, advanced: true },
        { key: "password", label: t("checks.field.password"), placeholder: "", hint: t("checks.field.passwordHint"), optional: true, advanced: true, input: "password" },
      ];
    },
    nameFrom: "url",
    severity: "critical",
    duration_s: 120,
    probe,
    anyRunner: !probe,
    moreAlerts: (metricOf, values) => {
      const rule = (suffix: string, comparison: Comparison, warning: number | null, critical: number | null, duration_s: number) => ({
        metric: metricOf(suffix),
        instance: null,
        comparison,
        warning,
        critical,
        duration_s,
        hysteresis: 0,
        enabled: true,
      });
      const https = (values.url ?? "").trim().toLowerCase().startsWith("https://");
      return [
        ...(https ? [rule("cert_ok", "<", null, 1, 300), rule("cert_days_left", "<", 14, 7, 0)] : []),
        rule("response_seconds", ">", 3, null, 300),
      ];
    },
  };
}

export const DEVICE_CHECK_TYPES: CheckType[] = [
  {
    kind: "ping",
    get label() {
      return t("checks.type.ping");
    },
    os: null,
    get fields() {
      return [hostField()];
    },
    nameFrom: "host",
    severity: "critical",
    duration_s: 120,
    probe: true,
  },
  {
    kind: "port",
    get label() {
      return t("checks.type.tcp");
    },
    os: null,
    get fields() {
      return [{ key: "port", label: t("checks.field.port"), placeholder: "443", check: portNumber }, hostField()];
    },
    nameFrom: "port",
    severity: "critical",
    duration_s: 60,
    probe: true,
  },
  {
    kind: "port",
    get label() {
      return t("checks.type.udp");
    },
    os: null,
    get fields() {
      return [
        { key: "port", label: t("checks.field.port"), placeholder: "53", check: portNumber, hint: t("devices.check.queryNote") },
        hostField(),
      ];
    },
    fixed: { protocol: "udp" },
    nameFrom: "port",
    prefix: "udp",
    severity: "critical",
    duration_s: 60,
    probe: true,
  },
  websiteType(true),
  ...appTypes(true),
];

/** Which checks a server can run; every check for a site, which can mix systems. */
export function checkTypesFor(osFamily: string | null): CheckType[] {
  if (osFamily === null) return CHECK_TYPES;
  const os = osFamily === "windows" ? "windows" : "linux";
  return CHECK_TYPES.filter((t) => t.os === null || t.os === os).map((t) => {
    const texts = t.on?.[os];
    return texts ? { ...t, fields: t.fields.map((f) => ({ ...f, ...texts[f.key] })) } : t;
  });
}

/** Longest sensor name the agent accepts (ADR-0012). */
const MAX_NAME = 64;

/** "service_nginx": a sensor name for the check, which prefixes its metrics. */
export function checkName(type: CheckType, values: Record<string, string>): string {
  const prefix = type.prefix ?? (type.kind === "systemd" || type.kind === "windows_service" ? "service" : type.kind);
  // A website by its host: "http_shop_example_com".
  const source =
    type.kind === "http"
      ? websiteHost((values[type.nameFrom] ?? "").trim())
      : ((values[type.nameFrom] ?? "").split(/[/\\]/).pop() ?? "");
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
    if (value === "" && field.optional) continue;
    const error = value === "" ? t("checks.error.field", { field: field.label.toLowerCase() }) : (field.check?.(value) ?? null);
    if (error) errors[field.key] = error;
  }
  return errors;
}

/** The sensor row, run every `intervalMs`, and the threshold that alerts on it. */
export function checkRequests(
  type: CheckType,
  values: Record<string, string>,
  intervalMs: number,
): { sensor: SensorConfigRequest; threshold: ThresholdRequest | null; more: ThresholdRequest[] } {
  const name = checkName(type, values);
  const params = Object.fromEntries(
    type.fields.map((f) => [f.key, (values[f.key] ?? "").trim()]).filter(([, value]) => value !== ""),
  );
  const sensor: SensorConfigRequest = { kind: type.kind, name, interval_ms: intervalMs, params: { ...params, ...type.fixed } };
  // A template's alerts come from the server (ADR-0183).
  if (type.template) return { sensor: { ...sensor, default_alerts: true }, threshold: null, more: [] };
  const event = EVENTS[type.kind];
  if (!event || type.noAlert) return { sensor, threshold: null, more: [] };
  const more = type.moreAlerts?.((suffix) => `${METRIC_PREFIX}${name}_${suffix}`, values) ?? [];
  const level = (severity: Severity) => (type.severity === severity ? event.value : null);
  return {
    sensor,
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
    more,
  };
}

/** Plain checks of the setting types a plugin's form has. */
const SETTING_CHECKS: Partial<Record<PluginSetting["type"], (value: string) => string | null>> = {
  url: (v) => (/^https?:\/\/[^\s/@]+/.test(v) ? null : t("checks.error.url")),
  number: (v) => (Number.isFinite(Number(v)) ? null : t("checks.error.numberOnly")),
  bool: (v) => (v === "true" || v === "false" ? null : t("checks.error.bool")),
};

/**
 * The check plugins turned on for the site `siteId` of `customerId`, as
 * kinds of check (ADR-0103). Their settings are the fields; the check is
 * named after the plugin and its first field.
 */
export function pluginCheckTypes(plugins: UsablePlugin[], siteId: string, customerId: string | null): CheckType[] {
  return plugins
    .filter((p) => p.kind === "check")
    .filter((p) => p.everywhere || p.site_ids.includes(siteId) || (customerId !== null && p.customer_ids.includes(customerId)))
    .map((p) => {
      const fields: CheckField[] = p.settings
        .filter((s) => s.type !== "secret")
        .map((s) => ({
          key: s.key,
          label: s.label,
          placeholder: s.default ?? "",
          initial: s.default,
          hint: s.help,
          optional: !s.required,
          check: SETTING_CHECKS[s.type],
        }));
      return {
        kind: "plugin",
        label: p.title,
        os: null,
        fields,
        fixed: { plugin: p.name },
        nameFrom: fields[0]?.key ?? "",
        prefix: p.name.replace(/-/g, "_"),
        severity: "warning",
        duration_s: 0,
        noAlert: true,
      };
    });
}

/**
 * Runs `sensor` every `ms` for `owner` (ADR-0097): changes `row`, the
 * owner's own row for it, or adds one that copies the inherited check.
 * `null` removes `row`, going back to the inherited check.
 */
export async function saveCheckInterval(
  owner: ConfigOwner,
  sensor: EffectiveSensor,
  row: SensorConfigEntry | undefined,
  ms: number | null,
): Promise<void> {
  if (ms === null) {
    if (row) await api.deleteSensor(row.id);
    return;
  }
  if (!row) {
    const { kind, name, instance, params } = sensor;
    await api.createSensor(owner, { kind, name, instance, interval_ms: ms, params, enabled: true });
    return;
  }
  // A timeout of its own must stay within the interval.
  const timeout_ms = row.timeout_ms !== null && row.timeout_ms <= ms ? row.timeout_ms : null;
  const { kind, name, instance, params, enabled } = row;
  await api.updateSensor(row.id, { kind, name, instance, interval_ms: ms, timeout_ms, params, enabled });
}

/** "Port 443", "Service nginx", "Disk space": a check in the list. */
export function checkTitle(sensor: EffectiveSensor): string {
  const title = ownTitle(sensor);
  // Run from the central server or a relay (ADR-0148): which one, as two probes may watch one address.
  if (!sensor.probe) return title;
  const probe = sensor.probe === "central" ? t("devices.from.short.central") : sensor.instance;
  return t("checks.title.from", { title, probe });
}

function ownTitle(sensor: EffectiveSensor): string {
  const { params } = sensor;
  switch (sensor.kind) {
    case "cpu":
      return t("checks.title.cpu");
    case "memory":
      return t("checks.title.memory");
    case "disk":
      return sensor.instance ? t("checks.title.diskOn", { instance: sensor.instance }) : t("checks.title.disk");
    case "diskio":
      return t("checks.title.diskio");
    case "log":
      return t("checks.title.log", { pattern: params.pattern ?? "", path: params.path ?? sensor.name });
    case "eventlog":
      return t("checks.title.eventlog", { channel: params.channel ?? "" });
    case "port": {
      const host = remoteHost(params);
      const port = params.port ?? "?";
      if (params.protocol === "udp") return host ? t("checks.title.udpOn", { port, host }) : t("checks.title.udp", { port });
      return host ? t("checks.title.portOn", { port, host }) : t("checks.title.port", { port });
    }
    case "ping":
      return params.host ? t("checks.title.pingOf", { host: params.host }) : t("checks.title.ping");
    case "snmp":
      return t(`devices.snmp.group.${SNMP_GROUPS.find((g) => g === params.what) ?? "system"}`);
    case "systemd":
      return t("checks.title.service", { name: params.units ?? sensor.name });
    case "windows_service":
      return t("checks.title.service", { name: params.services ?? sensor.name });
    case "script":
      return t("checks.title.script", { name: params.script ?? sensor.name });
    case "http":
      return t("checks.title.website", { host: websiteLabel(params.url) || sensor.name });
    case "vmware":
      return t("virtualization.check.vmware");
    case "hyperv":
      return t("virtualization.check.hyperv");
    case "aws":
      return t("cloud.check.aws");
    case "azure":
      return t("cloud.check.azure");
    case "postgresql":
    case "mysql":
    case "mssql":
    case "redis":
    case "jmx": {
      const app = t(APP_TITLES[sensor.kind]);
      const host = remoteHost(params);
      return host ? t("checks.title.appOn", { app, host }) : app;
    }
    case "docker":
      return t("checks.type.docker");
    case "kubernetes": {
      const host = remoteHost(params);
      return host ? t("checks.title.appOn", { app: t("checks.type.kubernetes"), host }) : t("checks.type.kubernetes");
    }
    default:
      return sensor.name.charAt(0).toUpperCase() + sensor.name.slice(1);
  }
}

/** The groups of SNMP checks (ADR-0149). */
export const SNMP_GROUPS = ["system", "interfaces", "host", "ups", "temperature", "printer"] as const;

/** "Warning" or "Critical", for the severity select; the label is read when shown. */
export const SEVERITIES = (["warning", "critical"] as const).map((value) => ({
  value,
  get label() {
    return SEVERITY_LABELS[value];
  },
}));
