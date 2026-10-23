import { useSearchParams } from "next/navigation";
import { useCallback } from "react";

/** Changes to the query: a string sets the key, null or "" removes it. */
export type ParamChanges = Record<string, string | null | undefined>;

/** The current location with `changes` applied to its query. */
export function hrefWith(changes: ParamChanges, location: Location = window.location): string {
  const params = new URLSearchParams(location.search);
  for (const [key, value] of Object.entries(changes)) {
    if (value === null || value === undefined || value === "") {
      params.delete(key);
    } else {
      params.set(key, value);
    }
  }
  const query = params.toString();
  return `${location.pathname}${query ? `?${query}` : ""}${location.hash}`;
}

/**
 * Writes `changes` into the URL with `history.replaceState`: no navigation,
 * no history entry, and Next's `useSearchParams` follows it.
 */
export function replaceParams(changes: ParamChanges) {
  // `null`, not Next's own state: Next ignores a replaceState carrying its
  // internal state, and useSearchParams would not follow.
  window.history.replaceState(null, "", hrefWith(changes));
}

/**
 * The page's query and a setter that merges changes into it, for filters
 * and sort orders that live in the URL so links are shareable. Needs a
 * `<Suspense>` boundary above it, like `useSearchParams`.
 */
export function useUrlParams(): [URLSearchParams, (changes: ParamChanges) => void] {
  const params = useSearchParams();
  const set = useCallback((changes: ParamChanges) => replaceParams(changes), []);
  return [params, set];
}
