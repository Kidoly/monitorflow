import type { IncidentQuery, IncidentSeverity, IncidentState, ThresholdSnapshot } from "./api";
import { UUID } from "./uuid";

/** What the incidents list shows; reflected in its query string. */
export interface IncidentFilters {
  state: IncidentState | "all";
  severity: IncidentSeverity | null;
  siteId: string | null;
  serverId: string | null;
}

export const NO_FILTERS: IncidentFilters = { state: "all", severity: null, siteId: null, serverId: null };

const STATES = ["open", "resolved", "all"] as const;
const SEVERITIES = ["warning", "critical"] as const;

function oneOf<T extends string>(values: readonly T[], value: string | null): T | null {
  return values.find((v) => v === value) ?? null;
}

function uuid(value: string | null): string | null {
  return value !== null && UUID.test(value) ? value.toLowerCase() : null;
}

/**
 * Filters from the page's query string. Unknown or malformed values are
 * ignored rather than sent to the API, which would refuse them.
 */
export function filtersFromQuery(params: URLSearchParams): IncidentFilters {
  return {
    state: oneOf(STATES, params.get("state")) ?? "all",
    severity: oneOf(SEVERITIES, params.get("severity")),
    siteId: uuid(params.get("site")),
    serverId: uuid(params.get("server")),
  };
}

/** The page's query string for `filters`, without defaults; `""` when unfiltered. */
export function filtersToQuery(filters: IncidentFilters): string {
  const params = new URLSearchParams();
  if (filters.state !== "all") params.set("state", filters.state);
  if (filters.severity) params.set("severity", filters.severity);
  if (filters.siteId) params.set("site", filters.siteId);
  if (filters.serverId) params.set("server", filters.serverId);
  return params.toString();
}

/** The API filters for `filters`. */
export function apiQuery(filters: IncidentFilters): IncidentQuery {
  return {
    ...(filters.state !== "all" && { state: filters.state }),
    ...(filters.severity && { severity: filters.severity }),
    ...(filters.serverId && { server_id: filters.serverId }),
    ...(filters.siteId && { site_id: filters.siteId }),
  };
}

/** Seconds an incident lasted, or has lasted so far when still open. */
export function incidentSeconds(openedAt: string, resolvedAt: string | null, now: Date): number {
  const end = resolvedAt ? Date.parse(resolvedAt) : now.getTime();
  return Math.max(0, Math.round((end - Date.parse(openedAt)) / 1000));
}

/** Steps for an incident's chart, the graphs' ones first (ADR-0020). */
const CHART_STEPS = [30, 120, 300, 1_800, 3_600, 21_600];
/** Most points in an incident's chart. */
const MAX_POINTS = 360;

/**
 * The time range of an incident's chart, in unix seconds: from a margin
 * before it opened (`marginS`, by default a quarter of its duration and at
 * least 15 minutes) to as long after it resolved, or to now. The step
 * keeps it to a few hundred points and the ends are aligned to it.
 */
export function incidentWindow(
  openedAt: string,
  resolvedAt: string | null,
  now: Date,
  marginS?: number,
): { start: number; end: number; step: number } {
  const opened = Date.parse(openedAt) / 1000;
  const until = Math.max(opened, (resolvedAt ? Date.parse(resolvedAt) : now.getTime()) / 1000);
  const margin = marginS ?? Math.max(900, (until - opened) / 4);
  const from = opened - margin;
  const to = resolvedAt ? Math.min(until + margin, now.getTime() / 1000) : until;
  const step = CHART_STEPS.find((s) => (to - from) / s <= MAX_POINTS) ?? 86_400;
  return { start: Math.floor(from / step) * step, end: Math.ceil(to / step) * step, step };
}

/** Margins of the incident chart's window buttons, "± 3 h" and so on. */
export const INCIDENT_MARGINS = [
  { label: "± 3 h", seconds: 10_800 },
  { label: "± 12 h", seconds: 43_200 },
  { label: "± 3 d", seconds: 259_200 },
] as const;

const RECOVERY: Record<ThresholdSnapshot["comparison"], string> = {
  ">": "at or below",
  ">=": "below",
  "<": "at or above",
  "<=": "above",
};

/**
 * When the engine resolves an incident under `threshold` (ADR-0017): once
 * the value is back past the lowest threshold set by the hysteresis, at
 * once. `format` writes values in the metric's unit. Null without a value.
 */
export function resolvesWhen(threshold: ThresholdSnapshot, format: (value: number) => string): string | null {
  const above = threshold.comparison === ">" || threshold.comparison === ">=";
  const lowest = threshold.warning ?? threshold.critical;
  if (lowest === null) return null;
  const limit = above ? lowest - threshold.hysteresis : lowest + threshold.hysteresis;
  return `the value is ${RECOVERY[threshold.comparison]} ${format(limit)}`;
}
