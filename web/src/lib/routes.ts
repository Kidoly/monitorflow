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
