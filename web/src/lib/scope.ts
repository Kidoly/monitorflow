import { useSearchParams } from "next/navigation";

import { UUID } from "./uuid";

/**
 * The customer (and optionally one of its sites) the user is looking at,
 * from `?customer=` and `?site=`; null is everything the roles allow. It is
 * a view filter only: the API decides what the user may read.
 */
export interface Scope {
  customerId: string | null;
  siteId: string | null;
}

export const NO_SCOPE: Scope = { customerId: null, siteId: null };

const id = (value: string | null) => (value !== null && UUID.test(value) ? value.toLowerCase() : null);

export function scopeFromQuery(params: URLSearchParams): Scope {
  const customerId = id(params.get("customer"));
  // A site without its customer is not a scope the switcher produces.
  return { customerId, siteId: customerId ? id(params.get("site")) : null };
}

/** The scope in the URL. Needs a `<Suspense>` boundary, like `useSearchParams`. */
export function useScope(): Scope {
  return scopeFromQuery(useSearchParams());
}

/** `href` carrying `scope`, so every page keeps it and links stay shareable. */
export function scopedHref(href: string, scope: Scope): string {
  if (!scope.customerId) return href;
  const [path, query = ""] = href.split("?", 2);
  const params = new URLSearchParams(query);
  params.set("customer", scope.customerId);
  if (scope.siteId) params.set("site", scope.siteId);
  return `${path}?${params}`;
}
