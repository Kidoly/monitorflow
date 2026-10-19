import type { IncidentQuery, IncidentSeverity, IncidentState } from "./api";
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
