"use client";

import { useCallback, useSyncExternalStore } from "react";

/**
 * Whether `query` matches, following changes. `fallback` where the
 * browser cannot tell (no `matchMedia`, as in tests) and while hydrating.
 */
export function useMediaQuery(query: string, fallback: boolean): boolean {
  const subscribe = useCallback(
    (listener: () => void) => {
      if (typeof window.matchMedia !== "function") return () => {};
      const list = window.matchMedia(query);
      list.addEventListener("change", listener);
      return () => list.removeEventListener("change", listener);
    },
    [query],
  );
  const read = () => (typeof window.matchMedia === "function" ? window.matchMedia(query).matches : fallback);
  return useSyncExternalStore(subscribe, read, () => fallback);
}
