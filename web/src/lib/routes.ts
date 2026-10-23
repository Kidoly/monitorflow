import { type IncidentFilters, NO_FILTERS, filtersToQuery } from "./incidents";

/** Link to the page of the server `id` (a query parameter, ADR-0020). */
export function serverHref(id: string): string {
  return `/servers/view/?id=${encodeURIComponent(id)}`;
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

export const MIRRORS_PATH = "/admin/mirrors/";

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
