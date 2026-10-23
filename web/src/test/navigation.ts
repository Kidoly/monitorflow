import { useMemo, useSyncExternalStore } from "react";
import { vi } from "vitest";

// A stand-in for `next/navigation` backed by jsdom's real location, so URL
// state written with `history.replaceState` reaches `useSearchParams` as it
// does in Next. Use it with:
//   vi.mock("next/navigation", () => import("@/test/navigation"));

const listeners = new Set<() => void>();
const notify = () => listeners.forEach((listener) => listener());

for (const method of ["pushState", "replaceState"] as const) {
  const original = window.history[method].bind(window.history);
  window.history[method] = (...args: Parameters<History["pushState"]>) => {
    original(...args);
    notify();
  };
}
window.addEventListener("popstate", notify);

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useSearchParams(): URLSearchParams {
  const search = useSyncExternalStore(subscribe, () => window.location.search);
  return useMemo(() => new URLSearchParams(search), [search]);
}

export function usePathname(): string {
  return useSyncExternalStore(subscribe, () => window.location.pathname);
}

/** Like Next's, the router object is stable across renders. */
export const router = {
  push: vi.fn((href: string) => window.history.pushState(null, "", href)),
  replace: vi.fn((href: string) => window.history.replaceState(null, "", href)),
  back: vi.fn(),
  forward: vi.fn(),
  refresh: vi.fn(),
  prefetch: vi.fn(),
};

export function useRouter() {
  return router;
}

/** Puts the test at `url` (path and query) without a navigation. */
export function setUrl(url: string) {
  window.history.replaceState(null, "", url);
}
