import { t } from "@/i18n";
import type {
  SnmpChannelRequest,
  SnmpCheck,
  SnmpCheckRequest,
  SnmpObject,
  SnmpReadObject,
  SnmpValueType,
} from "@/lib/api";
import { formatNumber } from "@/lib/format";
import type { Tone } from "@/components/ui/tone";

/** Most channels of one custom check (ADR-0157). */
export const MAX_CHANNELS = 10;

/** One channel as the form holds it, numbers as typed. */
export interface ChannelForm {
  oid: string;
  name: string;
  key?: string;
  type: SnmpValueType;
  unit: string;
  scale: "none" | "divide" | "multiply";
  factor: string;
  lookup: string;
  /** `""`: a text shown only. */
  match: string;
  expect: string;
  severity: "warning" | "critical";
  /** `""`: no limits. */
  comparison: "" | ">" | "<";
  warning: string;
  critical: string;
  durationS: number;
}

export interface CheckForm {
  name: string;
  probe: string;
  intervalMs: number;
  mode: "objects" | "table";
  table: string;
  nameColumn: string;
  channels: ChannelForm[];
}

export const NEW_CHANNEL: ChannelForm = {
  oid: "",
  name: "",
  type: "gauge",
  unit: "",
  scale: "none",
  factor: "",
  lookup: "",
  match: "",
  expect: "",
  severity: "critical",
  comparison: "",
  warning: "",
  critical: "",
  durationS: 0,
};

export const NEW_CHECK: CheckForm = {
  name: "snmp_custom",
  probe: "",
  intervalMs: 60_000,
  mode: "objects",
  table: "",
  nameColumn: "",
  channels: [NEW_CHANNEL],
};

const OID = /^\.?[0-2](\.\d+)+$/;

/** Whether `value` is a numeric OID. */
export const validOid = (value: string) => OID.test(value.trim());

/** The form of a stored check. */
export function formOf(check: SnmpCheck): CheckForm {
  return {
    name: check.name,
    probe: check.probe === "relay" ? (check.probe_relay_id ?? "") : (check.probe ?? ""),
    intervalMs: check.interval_ms,
    mode: check.table ? "table" : "objects",
    table: check.table ?? "",
    nameColumn: check.name_column ?? "",
    channels: check.channels.map((c) => ({
      oid: c.oid,
      name: c.name,
      key: c.key,
      type: c.type ?? "gauge",
      unit: c.unit ?? "",
      scale: c.divide ? "divide" : c.multiply ? "multiply" : "none",
      factor: String(c.divide ?? c.multiply ?? ""),
      lookup: c.lookup ?? "",
      match: c.match ?? "",
      expect: c.expect ?? "",
      severity: c.severity ?? "critical",
      comparison: c.limits ? (c.limits.comparison.startsWith("<") ? "<" : ">") : "",
      warning: c.limits?.warning != null ? String(c.limits.warning) : "",
      critical: c.limits?.critical != null ? String(c.limits.critical) : "",
      durationS: c.limits?.duration_s ?? 0,
    })),
  };
}

const number = (text: string): number | null | undefined => {
  if (text.trim() === "") return null;
  const n = Number(text.trim().replace(",", "."));
  return Number.isFinite(n) ? n : undefined;
};

/** What is wrong with the form, in words, or null. The API checks again. */
export function formProblem(form: CheckForm): string | null {
  if (!/^[a-z][a-z0-9_]{0,63}$/.test(form.name)) return t("snmp.error.name");
  const table = form.mode === "table" ? form.table.trim().replace(/^\./, "") : "";
  if (form.mode === "table" && !validOid(table)) return t("snmp.error.table");
  for (const [i, c] of form.channels.entries()) {
    const n = i + 1;
    if (!validOid(c.oid)) return t("snmp.error.oid", { n });
    if (table && !c.oid.trim().replace(/^\./, "").startsWith(`${table}.`)) return t("snmp.error.column", { n });
    if (c.name.trim() === "") return t("snmp.error.channelName", { n });
    if (c.scale !== "none") {
      const f = number(c.factor);
      if (f == null || f <= 0) return t("snmp.error.factor", { n });
    }
    if (c.type === "text" && c.match && c.match !== "equals" && c.expect === "") return t("snmp.error.expect", { n });
    if (c.comparison && c.type !== "text") {
      const w = number(c.warning);
      const k = number(c.critical);
      if (w === undefined || k === undefined || (w === null && k === null)) return t("snmp.error.limit", { n });
    }
  }
  return null;
}

/** The API's body for the form. */
export function requestOf(form: CheckForm): SnmpCheckRequest {
  const channels: SnmpChannelRequest[] = form.channels.map((c) => {
    const factor = number(c.factor) ?? undefined;
    const text = c.type === "text";
    const limits =
      c.comparison && !text
        ? {
            comparison: c.comparison,
            warning: number(c.warning) ?? null,
            critical: number(c.critical) ?? null,
            duration_s: c.durationS,
          }
        : null;
    return {
      oid: c.oid.trim(),
      name: c.name.trim(),
      ...(c.key && { key: c.key }),
      ...(c.type !== "gauge" && { type: c.type }),
      ...(c.unit.trim() && { unit: c.unit.trim() }),
      ...(!text && c.scale === "divide" && factor && { divide: factor }),
      ...(!text && c.scale === "multiply" && factor && { multiply: factor }),
      ...(c.type !== "counter" && c.lookup && { lookup: c.lookup }),
      ...(text && c.match && { match: c.match, expect: c.expect, severity: c.severity }),
      limits,
    };
  });
  return {
    name: form.name,
    ...(form.probe && { probe: form.probe }),
    interval_ms: form.intervalMs,
    ...(form.mode === "table" && { table: form.table.trim() }),
    ...(form.mode === "table" && form.nameColumn.trim() && { name_column: form.nameColumn.trim() }),
    channels,
  };
}

/** A channel for `object` of the library or a MIB: its OID read at `.0` for a scalar of a MIB. */
export function channelOf(object: SnmpObject): ChannelForm {
  const s = object.suggestion;
  const scalarOfMib = object.source === "mib" && object.kind === "scalar";
  const name = object.name.replace(/\.0$/, "");
  return {
    ...NEW_CHANNEL,
    oid: scalarOfMib ? `${object.oid}.0` : object.oid,
    name: name.charAt(0).toUpperCase() + name.slice(1),
    type: s.type,
    unit: s.unit,
    scale: s.divide !== 1 ? "divide" : s.multiply !== 1 ? "multiply" : "none",
    factor: s.divide !== 1 ? String(s.divide) : s.multiply !== 1 ? String(s.multiply) : "",
    lookup: s.lookup ?? "",
  };
}

/** A channel for what Browse found: a text, a counter or a value by its type. */
export function channelOfRead(object: SnmpReadObject): ChannelForm {
  const type: SnmpValueType = object.type.startsWith("Counter")
    ? "counter"
    : ["OctetString", "IpAddress", "ObjectIdentifier"].includes(object.type) && Number.isNaN(Number(object.value))
      ? "text"
      : "gauge";
  const name = (object.name ?? "").replace(/\.0$/, "");
  return { ...NEW_CHANNEL, oid: object.oid, name: name || object.oid, type };
}

/** "23.5 °C", "1,532 sessions". */
export function formatChannel(value: number | null, unit: string | undefined): string {
  if (value === null) return "";
  const shown = formatNumber(value, Math.abs(value) < 10 ? 2 : 1);
  return unit ? `${shown} ${unit}` : shown;
}

/** The tone and word of a state. */
export function stateOf(state: number | null): { tone: Tone; word: string } | null {
  if (state === null) return null;
  if (state >= 2) return { tone: "bad", word: t("snmp.state.critical") };
  if (state >= 1) return { tone: "warn", word: t("snmp.state.warning") };
  return { tone: "ok", word: t("snmp.state.ok") };
}
