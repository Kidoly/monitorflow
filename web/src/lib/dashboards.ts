import type {
  Combine,
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

import { metricTitle, shortMetric } from "./metrics";

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

export const PANEL_KINDS: readonly PanelKind[] = ["chart", "figure", "top", "alerts"];

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
  if (panel.kind === "alerts") return t("dashboards.title.alerts");
  const metric = panel.metric ? metricTitle(panel.metric) : t("dashboards.title.metric");
  const what = panel.instance && panel.instance !== "total" ? `${metric} ${panel.instance}` : metric;
  if (panel.kind === "top") {
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
  switch (kind) {
    case "chart":
      return {
        ...base,
        metric,
        ...(panel.instance !== undefined && { instance: panel.instance }),
        combine: panel.combine && CHART_COMBINES.includes(panel.combine) ? panel.combine : "each",
      };
    case "figure":
      return {
        ...base,
        metric,
        ...(panel.instance !== undefined && { instance: panel.instance }),
        combine: panel.combine && FIGURE_COMBINES.includes(panel.combine) ? panel.combine : "avg",
      };
    case "top":
      return {
        ...base,
        metric,
        ...(panel.instance !== undefined && { instance: panel.instance }),
        order: panel.order ?? (lowIsBad(metric) ? "lowest" : "highest"),
        count: panel.count ?? 5,
      };
    case "alerts":
      return { ...base, count: panel.count ?? 10 };
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

/** The ready-made dashboards, named in the active language. */
export const readyMades = (): ReadyMade[] => [
  {
    id: "overview",
    name: t("dashboards.ready.overview"),
    description: t("dashboards.ready.overviewText"),
    spec: {
      version: 1,
      range: "6h",
      panels: [
        panel("servers", { title: t("dashboards.ready.servers"), kind: "figure", width: 1, metric: CPU, instance: "total", combine: "count" }),
        panel("cpu", { title: t("dashboards.ready.cpu"), kind: "figure", width: 1, metric: CPU, instance: "total", combine: "avg" }),
        panel("disk", { title: t("dashboards.ready.disk"), kind: "figure", width: 1, metric: "monitorflow_disk_used_ratio", combine: "max" }),
        panel("cpu-lines", { title: t("dashboards.ready.cpuLines"), kind: "chart", width: 2, metric: CPU, instance: "total", combine: "each" }),
        panel("cpu-top", { title: t("dashboards.ready.cpuTop"), kind: "top", width: 1, metric: CPU, instance: "total", order: "highest", count: 5 }),
        panel("memory-lines", {
          title: t("dashboards.ready.memoryLines"),
          kind: "chart",
          width: 2,
          metric: "monitorflow_memory_available_ratio",
          combine: "each",
        }),
        panel("disk-top", { title: t("dashboards.ready.diskTop"), kind: "top", width: 1, metric: "monitorflow_disk_used_ratio", order: "highest", count: 5 }),
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
          kind: "chart",
          width: 2,
          metric: "monitorflow_memory_available_ratio",
          combine: "avg",
        }),
        panel("memory-low", {
          title: t("dashboards.ready.memoryLow"),
          kind: "top",
          width: 1,
          metric: "monitorflow_memory_available_ratio",
          order: "lowest",
          count: 5,
        }),
        panel("load", { title: t("dashboards.ready.load"), kind: "chart", width: 3, metric: "monitorflow_cpu_load15", combine: "each" }),
      ],
    },
  },
];

export function readyMade(id: string | null): ReadyMade | undefined {
  return readyMades().find((d) => d.id === id);
}
