/**
 * Moving from another monitoring tool (ADR-0146): what the preview's items
 * say in words, how they group, and which are left out once some are
 * unticked.
 */

import { type MessageKey, t } from "@/i18n";

import { checkTitle, describeThreshold, shapeOf } from "./alerts";
import { checkName, describeRule } from "./maintenance";
import type {
  EffectiveSensor,
  ImportItem,
  ImportList,
  ImportNote,
  ImportResult,
} from "./api";
import { formatDateTime } from "./format";

/** The lists of the preview, in the order shown. */
export const LISTS: readonly ImportList[] = [
  "exact",
  "approximated",
  "not_movable",
];

/** Kinds of item, in the order shown within a list. */
export const KINDS = [
  "server",
  "check",
  "threshold",
  "silence",
  "window",
  "channel",
  "builtin",
  "other",
] as const;

/** `t` for a key built at run time; the key itself when the catalog has none. */
const lookup = t as (key: string, values?: Record<string, string>) => string;

/** A note in the active language: the catalog's words, or the server's English for a code it does not know. */
export function noteText(note: ImportNote): string {
  const key = `imports.note.${note.code}`;
  const text = lookup(key, note.args ?? {});
  return text === key ? note.text : text;
}

/** What an item was in the source ("Trigger"), or the source's own word. */
export function originKind(kind: string): string {
  const key = `imports.origin.${kind}`;
  const text = lookup(key);
  return text === key ? kind : text;
}

/** The checks an import creates, as the sensors that name their alerts. */
function sensorsOf(items: ImportItem[]): EffectiveSensor[] {
  return items.flatMap((item) =>
    item.create?.object === "check"
      ? [
          {
            origin: "server",
            entry_id: null,
            kind: item.create.kind,
            name: item.create.name,
            instance: "",
            interval_ms: item.create.interval_ms,
            timeout_ms: null,
            params: item.create.params,
          } as EffectiveSensor,
        ]
      : [],
  );
}

/** One line saying what an item is, or creates. */
export function itemTitle(item: ImportItem, items: ImportItem[]): string {
  const create = item.create;
  if (!create) return item.origin.name;
  switch (create.object) {
    case "server":
      return create.hostname;
    case "device":
      return t("devices.importTitle", { name: create.hostname, address: create.address });
    case "check":
      return checkTitle({
        origin: "server",
        entry_id: null,
        kind: create.kind,
        name: create.name,
        instance: "",
        interval_ms: create.interval_ms,
        timeout_ms: null,
        params: create.params,
      } as EffectiveSensor);
    case "threshold": {
      const rule = {
        origin: "server",
        threshold_id: null,
        metric: create.metric,
        instance: create.instance,
        enabled: true,
        comparison: create.comparison,
        warning: create.warning,
        critical: create.critical,
        duration_s: create.duration_s,
        hysteresis: create.hysteresis,
      } as const;
      return describeThreshold(
        shapeOf(sensorsOf(items), create.metric, create.instance),
        rule,
      );
    }
    case "silence":
      return create.checks?.length
        ? t("imports.item.silenceChecks", {
            from: formatDateTime(create.starts_at),
            to: formatDateTime(create.ends_at),
            checks: create.checks.map(checkName).join(", "),
            reason: create.reason,
          })
        : t("imports.item.silence", {
            from: formatDateTime(create.starts_at),
            to: formatDateTime(create.ends_at),
            reason: create.reason,
          });
    case "window": {
      const rule = describeRule(create);
      return create.checks?.length
        ? t("imports.item.windowChecks", { rule, checks: create.checks.map(checkName).join(", "), reason: create.reason })
        : t("imports.item.window", { rule, reason: create.reason });
    }
    case "channel": {
      const settings = create.settings;
      const to = Array.isArray(settings.to) ? settings.to.join(", ") : "";
      const target = String(to || settings.url || settings.name || "");
      return t("imports.item.channel", {
        kind: channelKind(create.kind),
        target,
      });
    }
  }
}

/** "Email", "Webhook", "Slack": a channel's kind in words. */
export function channelKind(kind: string): string {
  switch (kind) {
    case "email":
      return t("imports.channel.email");
    case "webhook":
      return t("imports.channel.webhook");
    case "slack":
      return "Slack";
    case "teams":
      return "Teams";
    case "discord":
      return "Discord";
    default:
      return kind;
  }
}

/** Whether a person can tick or untick the item: it would create something. */
export const tickable = (item: ImportItem): boolean =>
  item.create !== undefined && !item.present && item.list !== "not_movable";

/**
 * The items left out: those unticked, and those that need one left out,
 * as the server decides. Items come in the order they are created, so one
 * pass sees each need before what needs it.
 */
export function leftOut(
  items: ImportItem[],
  unticked: ReadonlySet<string>,
): Set<string> {
  const out = new Set<string>();
  for (const item of items) {
    if (unticked.has(item.id) || item.needs.some((need) => out.has(need)))
      out.add(item.id);
  }
  return out;
}

/** How many items a confirmation creates once `out` is left out. */
export function toCreate(
  items: ImportItem[],
  out: ReadonlySet<string>,
): number {
  return items.filter((item) => tickable(item) && !out.has(item.id)).length;
}

/** The items of `list`, grouped by kind in the order shown. */
export function grouped(
  items: ImportItem[],
  list: ImportList,
): { kind: string; items: ImportItem[] }[] {
  const inList = items.filter((item) => item.list === list);
  const kinds: string[] = [...KINDS];
  for (const item of inList)
    if (!kinds.includes(item.kind)) kinds.push(item.kind);
  return kinds
    .map((kind) => ({
      kind,
      items: inList.filter((item) => item.kind === kind),
    }))
    .filter((group) => group.items.length > 0);
}

/** A group's heading: "Servers", "Checks", … */
export function kindHeading(kind: string): string {
  const key = `imports.kind.${kind}`;
  const text = lookup(key);
  return text === key ? kind : text;
}

/** "2 servers, 5 checks": what a result created or removed. */
export function countsText(counts: Record<string, number>): string {
  const parts = KINDS.flatMap((kind) => {
    const count = counts[kind] ?? 0;
    if (count === 0) return [];
    const key = `imports.count.${kind}` as MessageKey;
    return [
      (t as (key: string, values: { count: number }) => string)(key, { count }),
    ];
  });
  return parts.length > 0 ? parts.join(", ") : t("imports.count.nothing");
}

/** Whether the result created anything. */
export const createdAnything = (result: ImportResult): boolean =>
  Object.values(result.created).some((n) => n > 0);
