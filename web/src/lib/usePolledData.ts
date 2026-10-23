import { useCallback, useState } from "react";

import { isAbortError } from "./api";
import { usePolling } from "./usePolling";

/** Every page refreshes on this interval (one timer per page). */
export const REFRESH_MS = 15_000;

export interface Polled<T> {
  /** The last payload received; kept when a later refresh fails. */
  data: T | undefined;
  /** The last failure, cleared by the next success. */
  error: unknown;
  /** A refresh failed after an earlier success: show the stale banner, keep the table. */
  stale: boolean;
  /** Failures in a row since the last success. */
  failures: number;
  /** When `data` arrived. */
  updatedAt: Date | null;
  /** Fetches now and restarts the interval. */
  refresh: () => void;
}

interface State<T> {
  data: T | undefined;
  error: unknown;
  failures: number;
  updatedAt: Date | null;
}

/**
 * `fetcher` now and every `intervalMs` (15 s) while the tab is visible.
 * On failure the last payload stays and `stale` turns true, so the page
 * shows the banner and leaves its table untouched. `fetcher` must be stable
 * (`useCallback`); a new one fetches at once, keeping the old payload until
 * the new one arrives.
 */
export function usePolledData<T>(
  fetcher: (signal: AbortSignal) => Promise<T>,
  { intervalMs = REFRESH_MS, enabled = true }: { intervalMs?: number; enabled?: boolean } = {},
): Polled<T> {
  const [state, setState] = useState<State<T>>({ data: undefined, error: null, failures: 0, updatedAt: null });
  const [nonce, setNonce] = useState(0);

  const load = useCallback(
    async (signal: AbortSignal) => {
      void nonce;
      try {
        const data = await fetcher(signal);
        if (!signal.aborted) setState({ data, error: null, failures: 0, updatedAt: new Date() });
      } catch (error) {
        if (signal.aborted || isAbortError(error)) return;
        setState((s) => ({ ...s, error, failures: s.failures + 1 }));
      }
    },
    [fetcher, nonce],
  );
  usePolling(load, intervalMs, enabled);

  const refresh = useCallback(() => setNonce((n) => n + 1), []);
  return {
    ...state,
    stale: state.error !== null && state.updatedAt !== null,
    refresh,
  };
}
