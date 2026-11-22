import type {
  Combine,
  ServerState,
  DashboardSpec,
  Panel,
  PanelKind,
  PanelServers,
  Server,
  Sharing,
  TimeRangeLabel,
  Within,
} from "./api";
import { t } from "@/i18n";

import { type Unit, formatNumber, formatValue, unitOf } from "./format";
import { metricTitle, shortMetric } from "./metrics";
import { formatSensorValue } from "./sensors";

/** Dashboards are looked at rather than watched, so they refresh less often (ADR-0095). */
export const DASHBOARD_REFRESH_MS = 30_000;
export const MAX_PANELS = 24;
export const MAX_NAMED_SERVERS = 50;
export const MAX_NAME = 80;
/** Lines of a chart showing each server. */
export const EACH_LINES = 8;

export const RANGES: readonly TimeRangeLabel[] = ["1h", "6h", "24h", "7d", "30d"];

/** "1 h", "7 d" ("7 j" in French). */
export function rangeLabel(range: TimeRangeLabel): string {
  const amount = range.slice(0, -1);
  return `${amount} ${t(range.endsWith("d") ? "format.unit.day" : "format.unit.hour")}`;
}

export function isRange(value: string | null): value is TimeRangeLabel {
  return value !== null && (RANGES as readonly string[]).includes(value);
}

export const PANEL_KINDS: readonly PanelKind[] = [
  "chart",
  "area",
  "columns",
  "heatmap",
  "figure",
  "gauge",
  "top",
  "bars",
  "table",
  "states",
  "pie",
  "timeline",
  "alerts",
  "text",
];

/** The colour of a server's state on dashboards: alarming ones loud, quiet ones grey. */
export function stateColor(state: ServerState): string {
  switch (state) {
    case "critical":
      return "var(--threshold-crit)";
    case "warning":
      return "var(--threshold-warn)";
    case "acknowledged":
      return "var(--series-1)";
    case "ok":
      return "var(--series-3)";
    case "offline":
      return "var(--muted)";
    default:
      return "var(--border-strong)";
  }
}

/** Most rows of a table. */
export const MAX_TABLE_ROWS = 50;
/** Most columns of a table. */
export const MAX_TABLE_COLUMNS = 6;
/** Longest text of a text panel. */
export const MAX_TEXT = 4000;

/** The kind whose data a panel draws: area and columns a chart's, a gauge a figure's, bars a top list's. */
export function baseKind(kind: PanelKind): "chart" | "figure" | "top" | "alerts" | "table" | "text" | "states" | "pie" | "timeline" {
  switch (kind) {
    case "area":
    case "columns":
    case "heatmap":
      return "chart";
    case "gauge":
      return "figure";
    case "bars":
      return "top";
    default:
      return kind;
  }
}

export const kindLabel = (kind: PanelKind): string => t(`dashboards.kind.${kind}`);

/** How servers combine, in the panel form. */
export const combineLabel = (combine: Combine): string =>
  combine === "each" ? t("dashboards.combine.each", { lines: EACH_LINES }) : t(`dashboards.combine.${combine}`);

export const CHART_COMBINES: Combine[] = ["each", "avg", "max", "min", "sum"];
export const FIGURE_COMBINES: Combine[] = ["avg", "max", "min", "sum", "count"];

export const widthLabel = (width: Panel["width"]): string => t(`dashboards.width.${width}`);

export const CPU = "monitorflow_cpu_usage_ratio";

/**
 * Whether a low value of `metric` is the bad one (available, free, up), as
 * the API decides which lines of "one line per server" to draw.
 */
export function lowIsBad(metric: string): boolean {
  const short = shortMetric(metric);
  return /available|free|_(up|active|loaded|installed)$/.test(short);
}

/** How a panel writes its values: its unit, else the metric's. */
export function panelUnit(panel: Panel): Unit {
  return panel.unit ?? unitOf(panel.metric ?? "");
}

/** Whether the panel's values add up across servers (bytes, traffic, counts), so areas and columns stack. */
export function panelAdditive(panel: Panel): boolean {
  return !["ratio", "celsius", "seconds"].includes(panelUnit(panel));
}

/** Value to text for a panel: "how many servers" is a count, the rest in its unit. */
export function panelFormat(panel: Panel): (value: number) => string {
  if (baseKind(panel.kind) === "figure" && panel.combine === "count") return (v) => formatNumber(v);
  if (panel.unit) return (v) => formatValue(v, panel.unit!);
  return (v) => formatSensorValue(panel.metric ?? "", v);
}

export type Level = "ok" | "warning" | "critical";

/**
 * Where `value` stands against the panel's thresholds: above them, or
 * below for metrics where low is bad; null without thresholds.
 */
export function levelOf(panel: Panel, value: number | null): Level | null {
  const limits = panel.thresholds;
  if (!limits || value === null) return null;
  const low = lowIsBad(panel.metric ?? "");
  const beyond = (limit: number | undefined) => limit !== undefined && (low ? value <= limit : value >= limit);
  if (beyond(limits.critical)) return "critical";
  if (beyond(limits.warning)) return "warning";
  return "ok";
}

/** The `index`th colour of the panel's palette, from its first colour. */
export function panelColor(panel: Panel, index = 0): string {
  return `var(--series-${(((panel.color ?? 1) - 1 + index) % 8) + 1})`;
}

/** A value's colour: its threshold's when it is beyond one, else the panel's. */
export function levelColor(panel: Panel, value: number | null, index = 0): string {
  switch (levelOf(panel, value)) {
    case "critical":
      return "var(--threshold-crit)";
    case "warning":
      return "var(--threshold-warn)";
    default:
      return panelColor(panel, index);
  }
}

/**
 * The scale of a gauge or bars: the panel's, else 0 to 1 for ratios, else
 * from 0 (or the lowest value) to the highest value or threshold.
 */
export function panelScale(panel: Panel, values: (number | null)[]): { min: number; max: number } {
  const known = values.filter((v): v is number => v !== null && Number.isFinite(v));
  const ratio = panelUnit(panel) === "ratio";
  const limits = [panel.thresholds?.warning, panel.thresholds?.critical].filter((v): v is number => v !== undefined);
  const min = panel.min ?? (ratio ? 0 : Math.min(0, ...known));
  const top = Math.max(...known, ...limits, min + 1);
  const max = panel.max ?? (ratio ? Math.max(1, top) : top);
  return { min, max: max > min ? max : min + 1 };
}

/**
 * The thresholds as coloured stretches of a gauge's scale: amber from the
 * warning to the critical value, red beyond, or below for low-is-bad metrics.
 */
export function thresholdBands(panel: Panel, min: number, max: number): { from: number; to: number; color: string }[] {
  const { warning, critical } = panel.thresholds ?? {};
  const warn = "var(--threshold-warn)";
  const crit = "var(--threshold-crit)";
  if (lowIsBad(panel.metric ?? "")) {
    return [
      ...(critical !== undefined ? [{ from: min, to: critical, color: crit }] : []),
      ...(warning !== undefined ? [{ from: critical ?? min, to: warning, color: warn }] : []),
    ];
  }
  return [
    ...(warning !== undefined ? [{ from: warning, to: critical ?? max, color: warn }] : []),
    ...(critical !== undefined ? [{ from: critical, to: max, color: crit }] : []),
  ];
}

/** A heatmap cell's colour: its threshold's past one, else the panel's, stronger as the value rises. */
export function heatColor(panel: Panel, min: number, max: number): (value: number) => { background: string; opacity: number } {
  return (value) => {
    const level = levelOf(panel, value);
    if (level === "critical" || level === "warning") return { background: levelColor(panel, value), opacity: 1 };
    const share = Math.min(Math.max((value - min) / (max - min || 1), 0), 1);
    return { background: panelColor(panel), opacity: 0.22 + 0.78 * (lowIsBad(panel.metric ?? "") ? 1 - share : share) };
  };
}

/** Series worst first: the highest peak, or the lowest dip where low is bad. */
export function worstFirst<T extends { points: [number, number | null][] }>(series: T[], metric: string): T[] {
  const low = lowIsBad(metric);
  const peak = (s: T) => {
    const values = s.points.map((p) => p[1]).filter((v): v is number => v !== null);
    return values.length ? (low ? -Math.min(...values) : Math.max(...values)) : -Infinity;
  };
  return [...series].sort((a, b) => peak(b) - peak(a));
}

/** A short random panel id, unique within a dashboard. */
export function newPanelId(taken: Iterable<string> = []): string {
  const used = new Set(taken);
  for (;;) {
    const id = Math.random().toString(36).slice(2, 10);
    if (id && !used.has(id)) return id;
  }
}

/** What a panel watches by default: the total when there is one, else everything. */
export function defaultInstance(instances: string[]): string | undefined {
  return instances.includes("total") ? "total" : undefined;
}

/** A title from a panel's choices: "CPU usage total, average", "Fullest: Disk used". */
export function suggestedTitle(panel: Panel): string {
  if (baseKind(panel.kind) === "alerts") return t("dashboards.title.alerts");
  if (panel.kind === "text") return t("dashboards.title.text");
  if (panel.kind === "table") return t("dashboards.title.table");
  if (panel.kind === "timeline") return t("dashboards.title.timeline");
  if (panel.kind === "states" && !panel.metric) return t("dashboards.title.states");
  if (panel.kind === "pie" && !panel.metric) return t("dashboards.title.pie");
  const metric = panel.metric ? metricTitle(panel.metric) : t("dashboards.title.metric");
  const what = panel.instance && panel.instance !== "total" ? `${metric} ${panel.instance}` : metric;
  if (baseKind(panel.kind) === "top") {
    return panel.order === "lowest" ? t("dashboards.title.lowest", { what }) : t("dashboards.title.highest", { what });
  }
  switch (panel.combine) {
    case "count":
      return t("dashboards.title.count", { what: what.toLowerCase() });
    case "avg":
    case "max":
    case "min":
    case "sum":
      return t(`dashboards.title.${panel.combine}`, { what });
    default:
      return what;
  }
}

/**
 * The panel as `kind`, keeping its servers, metric, width and title, and
 * giving the fields of that kind their defaults; the others are dropped.
 */
export function withKind(panel: Panel, kind: PanelKind): Panel {
  const base: Panel = { id: panel.id, title: panel.title, kind, width: panel.width, servers: panel.servers };
  const metric = panel.metric ?? CPU;
  const instance = panel.instance !== undefined ? { instance: panel.instance } : {};
  // Display options follow the panel, except a legend, which only charts over time have.
  const options: Partial<Panel> =
    kind === "alerts" || kind === "table" || kind === "text" || kind === "timeline" || (!panel.metric && (kind === "states" || kind === "pie"))
      ? {}
      : {
          ...(panel.unit && { unit: panel.unit }),
          ...(panel.min !== undefined && { min: panel.min }),
          ...(panel.max !== undefined && { max: panel.max }),
          ...(panel.thresholds && { thresholds: panel.thresholds }),
          ...(panel.color !== undefined && { color: panel.color }),
          ...(panel.legend !== undefined && ["chart", "area", "columns"].includes(kind) && { legend: panel.legend }),
        };
  // States and pies show the servers' states, or a metric when the panel has one.
  if (kind === "states" || kind === "pie") return panel.metric ? { ...base, metric: panel.metric, ...instance, ...options } : base;
  if (kind === "timeline") return { ...base, count: Math.min(panel.count ?? 10, 20) };
  if (kind === "heatmap") return { ...base, metric, ...instance, count: Math.min(panel.count ?? 10, 20), ...options };
  switch (baseKind(kind)) {
    case "chart":
      return {
        ...base,
        metric,
        ...instance,
        combine: panel.combine && CHART_COMBINES.includes(panel.combine) ? panel.combine : "each",
        ...options,
      };
    case "figure":
      return {
        ...base,
        metric,
        ...instance,
        combine: panel.combine && FIGURE_COMBINES.includes(panel.combine) ? panel.combine : kind === "gauge" ? "max" : "avg",
        ...options,
      };
    case "top":
      return {
        ...base,
        metric,
        ...instance,
        order: panel.order ?? (lowIsBad(metric) ? "lowest" : "highest"),
        count: panel.count ?? 5,
        ...options,
      };
    case "alerts":
      return { ...base, count: Math.min(panel.count ?? 10, 20) };
    case "table":
      return {
        ...base,
        columns: panel.columns ?? [{ metric, ...instance }],
        order: panel.order ?? (lowIsBad(metric) ? "lowest" : "highest"),
        count: panel.count ?? 20,
      };
    case "text":
      return { ...base, servers: { kind: "all" }, text: panel.text ?? t("dashboards.text.example") };
    // Handled above.
    case "states":
    case "pie":
    case "timeline":
      return base;
  }
}

/** A new chart of every server's CPU, the starting point of the panel form. */
export function newPanel(taken: Iterable<string> = []): Panel {
  const panel = withKind(
    { id: newPanelId(taken), title: "", kind: "chart", width: 2, servers: { kind: "all" }, instance: "total" },
    "chart",
  );
  return { ...panel, title: suggestedTitle(panel) };
}

/** `panels` with the one at `index` moved by `delta` places, within bounds. */
export function movePanel<T>(panels: T[], index: number, delta: number): T[] {
  const to = Math.min(Math.max(index + delta, 0), panels.length - 1);
  if (to === index) return panels;
  const next = [...panels];
  const [moved] = next.splice(index, 1);
  next.splice(to, 0, moved!);
  return next;
}

/** `panels` with the one at `from` put at `to`, as a drag and drop does. */
export function placePanel<T>(panels: T[], from: number, to: number): T[] {
  return movePanel(panels, from, to - from);
}

/**
 * The spec without the chosen servers `known` does not hold (removed, or
 * no longer visible); a panel left with none shows every server instead.
 */
export function dropMissingServers(spec: DashboardSpec, known: ReadonlySet<string>): { spec: DashboardSpec; dropped: number } {
  let dropped = 0;
  const panels = spec.panels.map((panel) => {
    if (panel.servers.kind !== "list") return panel;
    const ids = panel.servers.ids.filter((id) => known.has(id));
    dropped += panel.servers.ids.length - ids.length;
    if (ids.length === panel.servers.ids.length) return panel;
    const servers: PanelServers = ids.length ? { kind: "list", ids } : { kind: "all" };
    return { ...panel, servers };
  });
  return { spec: { ...spec, panels }, dropped };
}

/** What a dashboard so shared can show at most. */
export function withinOf(sharing: Sharing): Within {
  // Only the kind and id: the API refuses other fields, like a display name.
  return sharing.kind === "personal" ? { kind: "all" } : { kind: sharing.kind, id: sharing.id };
}

/** "Only me", "Site paris", "Customer acme" ("All sites" for a single company, ADR-0104). */
export function sharingLabel(sharing: Sharing & { name?: string }, multiCustomer = true): string {
  switch (sharing.kind) {
    case "personal":
      return t("dashboards.sharing.personal");
    case "site":
      return t("dashboards.sharing.site", { name: sharing.name ?? "" }).trim();
    case "customer":
      return multiCustomer
        ? t("dashboards.sharing.customer", { name: sharing.name ?? "" }).trim()
        : t("dashboards.sharing.allSites");
  }
}

/**
 * A spec to keep as one's own: "every server of this dashboard" becomes
 * the site or customer it was shared with, so the copy shows the same
 * servers.
 */
export function copySpec(spec: DashboardSpec, within: Within): DashboardSpec {
  if (within.kind === "all") return spec;
  return {
    ...spec,
    panels: spec.panels.map((p) => (p.servers.kind === "all" ? { ...p, servers: { kind: within.kind, id: within.id } } : p)),
  };
}

export function copyName(name: string): string {
  return t("dashboards.copyOf", { name }).slice(0, MAX_NAME);
}

/** The sites and customers of `servers`, by name, for the panel form. */
export function placesOf(servers: Server[]): {
  sites: { id: string; name: string; customerId: string }[];
  customers: { id: string; name: string }[];
} {
  const sites = new Map<string, { id: string; name: string; customerId: string }>();
  const customers = new Map<string, { id: string; name: string }>();
  for (const s of servers) {
    sites.set(s.site_id, { id: s.site_id, name: s.site_name, customerId: s.customer_id });
    customers.set(s.customer_id, { id: s.customer_id, name: s.customer_name });
  }
  const byName = (a: { name: string }, b: { name: string }) => a.name.localeCompare(b.name);
  return { sites: [...sites.values()].sort(byName), customers: [...customers.values()].sort(byName) };
}

/** The servers a dashboard so shared can name: its site's, its customer's, or all. */
export function serversWithin(servers: Server[], within: Within): Server[] {
  return servers.filter(
    (s) =>
      !s.revoked_at &&
      (within.kind === "all" || (within.kind === "site" ? s.site_id === within.id : s.customer_id === within.id)),
  );
}

/** A dashboard MonitorFlow comes with: not stored, the same for everyone. */
export interface ReadyMade {
  id: string;
  name: string;
  description: string;
  spec: DashboardSpec;
}

const all: PanelServers = { kind: "all" };
const panel = (id: string, fields: Omit<Panel, "id" | "servers">): Panel => ({ id, servers: all, ...fields });

/** The ready-made dashboards, named in the active language; also the layouts a new dashboard starts from. */
export const readyMades = (): ReadyMade[] => [
  {
    id: "health",
    name: t("dashboards.ready.health"),
    description: t("dashboards.ready.healthText"),
    spec: {
      version: 1,
      range: "24h",
      panels: [
        panel("states", { title: t("dashboards.title.states"), kind: "states", width: 2 }),
        panel("by-state", { title: t("dashboards.title.pie"), kind: "pie", width: 1 }),
        panel("timeline", { title: t("dashboards.title.timeline"), kind: "timeline", width: 3, count: 10 }),
        panel("cpu-heat", {
          title: t("dashboards.ready.cpuHeat"),
          kind: "heatmap",
          width: 2,
          metric: CPU,
          instance: "total",
          count: 10,
          thresholds: { warning: 0.8, critical: 0.95 },
        }),
        panel("alerts", { title: t("dashboards.title.alerts"), kind: "alerts", width: 1, count: 10 }),
      ],
    },
  },
  {
    id: "overview",
    name: t("dashboards.ready.overview"),
    description: t("dashboards.ready.overviewText"),
    spec: {
      version: 1,
      range: "6h",
      panels: [
        panel("servers", { title: t("dashboards.ready.servers"), kind: "figure", width: 1, metric: CPU, instance: "total", combine: "count" }),
        panel("cpu", {
          title: t("dashboards.ready.cpu"),
          kind: "gauge",
          width: 1,
          metric: CPU,
          instance: "total",
          combine: "avg",
          thresholds: { warning: 0.8, critical: 0.95 },
        }),
        panel("disk", {
          title: t("dashboards.ready.disk"),
          kind: "gauge",
          width: 1,
          metric: "monitorflow_disk_used_ratio",
          combine: "max",
          thresholds: { warning: 0.8, critical: 0.9 },
        }),
        panel("cpu-lines", { title: t("dashboards.ready.cpuLines"), kind: "chart", width: 2, metric: CPU, instance: "total", combine: "each" }),
        panel("cpu-top", { title: t("dashboards.ready.cpuTop"), kind: "bars", width: 1, metric: CPU, instance: "total", order: "highest", count: 5 }),
        panel("memory-lines", {
          title: t("dashboards.ready.memoryLines"),
          kind: "chart",
          width: 2,
          metric: "monitorflow_memory_available_ratio",
          combine: "each",
        }),
        panel("disk-top", {
          title: t("dashboards.ready.diskTop"),
          kind: "bars",
          width: 1,
          metric: "monitorflow_disk_used_ratio",
          order: "highest",
          count: 5,
          thresholds: { warning: 0.8, critical: 0.9 },
        }),
        panel("alerts", { title: t("dashboards.title.alerts"), kind: "alerts", width: 3, count: 10 }),
      ],
    },
  },
  {
    id: "capacity",
    name: t("dashboards.ready.capacity"),
    description: t("dashboards.ready.capacityText"),
    spec: {
      version: 1,
      range: "7d",
      panels: [
        panel("disk-lines", { title: t("dashboards.ready.diskLines"), kind: "chart", width: 2, metric: "monitorflow_disk_used_ratio", combine: "each" }),
        panel("disk-free", {
          title: t("dashboards.ready.diskFree"),
          kind: "top",
          width: 1,
          metric: "monitorflow_disk_available_bytes",
          order: "lowest",
          count: 5,
        }),
        panel("memory-avg", {
          title: t("dashboards.ready.memoryAvg"),
          kind: "area",
          width: 2,
          metric: "monitorflow_memory_available_ratio",
          combine: "avg",
        }),
        panel("memory-low", {
          title: t("dashboards.ready.memoryLow"),
          kind: "bars",
          width: 1,
          metric: "monitorflow_memory_available_ratio",
          order: "lowest",
          count: 5,
          thresholds: { warning: 0.2, critical: 0.1 },
        }),
        panel("load", { title: t("dashboards.ready.load"), kind: "chart", width: 3, metric: "monitorflow_cpu_load15", combine: "each" }),
      ],
    },
  },
];

/**
 * A layout to start a new dashboard from: the ready-made dashboard's
 * panels and range, kept as they are.
 */
export function starter(id: string): DashboardSpec | undefined {
  return readyMade(id)?.spec;
}

export function readyMade(id: string | null): ReadyMade | undefined {
  return readyMades().find((d) => d.id === id);
}

/** What a panel shows, the first choice of the panel form (ADR-0166). */
export type PanelSource = "metric" | "states" | "alerts" | "text";

export function sourceOf(panel: Panel): PanelSource {
  if (panel.kind === "text") return "text";
  if (panel.kind === "alerts" || panel.kind === "timeline") return "alerts";
  if ((panel.kind === "states" || panel.kind === "pie") && !panel.metric) return "states";
  return "metric";
}

/** The ways to draw what a panel shows, the most useful first. */
export const KINDS_FOR: Record<PanelSource, readonly PanelKind[]> = {
  metric: ["chart", "area", "columns", "heatmap", "gauge", "figure", "bars", "top", "states", "pie", "table"],
  states: ["states", "pie"],
  alerts: ["alerts", "timeline"],
  text: ["text"],
};

/** The metrics most dashboards show, offered first. */
export const COMMON_METRICS = [
  "monitorflow_cpu_usage_ratio",
  "monitorflow_memory_available_ratio",
  "monitorflow_disk_used_ratio",
  "monitorflow_cpu_load15",
  "monitorflow_diskio_busy_ratio",
  "monitorflow_ping_rtt_seconds",
];

/** How a metric is best drawn at first: up or down as squares, the rest as lines over time. */
export function defaultKind(metric: string): PanelKind {
  return /_(up|active)$/.test(shortMetric(metric)) ? "states" : "chart";
}

/**
 * The panel showing `source` instead, drawn its first way, or `metric`
 * drawn as it is now when it already shows a metric.
 */
export function withSource(panel: Panel, source: PanelSource, metric?: string): Panel {
  if (source === "metric") {
    const { instance: _instance, metric: _metric, ...rest } = panel;
    void _instance;
    void _metric;
    const next = metric ?? panel.metric ?? CPU;
    const kind = sourceOf(panel) === "metric" && panel.kind !== "table" ? panel.kind : defaultKind(next);
    return withKind({ ...rest, metric: next, ...(next === CPU && { instance: "total" }), columns: undefined }, kind);
  }
  const { metric: _m, instance: _i, ...rest } = panel;
  void _m;
  void _i;
  return withKind(rest, KINDS_FOR[source][0]!);
}
