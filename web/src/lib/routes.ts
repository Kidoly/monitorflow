import { type IncidentFilters, NO_FILTERS, filtersToQuery } from "./incidents";
import { shortMetric } from "./metrics";

/** Link to the page of the server `id` (a query parameter, ADR-0020), on `tab` when given. */
export function serverHref(id: string, tab?: "sensors" | "alerts" | "updates" | "commands"): string {
  return `/servers/view/?id=${encodeURIComponent(id)}${tab ? `&tab=${tab}` : ""}`;
}

/** Link to the page of the site `id`: its alerts and checks. */
export function siteHref(id: string): string {
  return `/sites/view/?id=${encodeURIComponent(id)}`;
}

/** Link to the page of one sensor's metric on the server `id`; `instance` "" is none. */
export function sensorHref(id: string, metric: string, instance = ""): string {
  const params = new URLSearchParams({ id, sensor: shortMetric(metric) });
  if (instance) params.set("instance", instance);
  return `/servers/sensor/?${params}`;
}

/** Link to the incidents list with `filters` (see `lib/incidents.ts`). */
export function incidentsHref(filters: Partial<IncidentFilters> = {}): string {
  const query = filtersToQuery({ ...NO_FILTERS, ...filters });
  return query ? `/incidents/?${query}` : "/incidents/";
}

/** Link to the page of the incident `id` (a query parameter, like servers). */
export function incidentHref(id: string): string {
  return `/incidents/view/?id=${encodeURIComponent(id)}`;
}

/** Administration pages (ADR-0029). */
export const RELAYS_PATH = "/admin/relays/";
export const SITES_PATH = "/admin/sites/";
export const TOKENS_PATH = "/admin/tokens/";
export const USERS_PATH = "/admin/users/";

export const MIRRORS_PATH = "/admin/mirrors/";
/** Automatic agent and relay updates (ADR-0055). */
export const AUTO_UPDATE_PATH = "/admin/updates/";

/** Email and webhook notifications (ADR-0058). */
export const NOTIFICATIONS_PATH = "/admin/notifications/";

/** Updating the central server (ADR-0056). */
export const SERVER_UPDATE_PATH = "/admin/server-update/";

/** Link to the page of the mirror `id`. */
export function mirrorHref(id: string): string {
  return `/admin/mirrors/view/?id=${encodeURIComponent(id)}`;
}

/** Link to the page of the relay `id`. */
export function relayHref(id: string): string {
  return `/admin/relays/view/?id=${encodeURIComponent(id)}`;
}

/** Update campaigns (ADR-0033). */
export const CAMPAIGNS_PATH = "/campaigns/";
export const NEW_CAMPAIGN_PATH = "/campaigns/new/";

/** Link to the page of the campaign `id`. */
export function campaignHref(id: string): string {
  return `/campaigns/view/?id=${encodeURIComponent(id)}`;
}

/** The approval queue and the remediation audit log (ADR-0045). */
export const APPROVALS_PATH = "/remediation/approvals/";
export const AUDIT_PATH = "/remediation/audit/";

/** Link to the remediation audit log, filtered to one incident when given. */
export function auditHref(filters: { incident?: string } = {}): string {
  return filters.incident ? `${AUDIT_PATH}?incident=${encodeURIComponent(filters.incident)}` : AUDIT_PATH;
}
