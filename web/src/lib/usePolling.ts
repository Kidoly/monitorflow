import { useEffect } from "react";

/**
 * Calls `load` now and every `intervalMs` while the tab is visible. A hidden
 * tab stops the timer and aborts the request in flight; showing it again
 * loads at once. `load` should be stable (`useCallback`): a new one restarts
 * the cycle, which is how a changed parameter triggers an immediate reload.
 */
export function usePolling(
  load: (signal: AbortSignal) => Promise<void>,
  intervalMs: number,
  enabled = true,
) {
  useEffect(() => {
    if (!enabled) return;
    let controller: AbortController | undefined;
    let timer: ReturnType<typeof setInterval> | undefined;
    const run = () => {
      controller?.abort();
      controller = new AbortController();
      void load(controller.signal);
    };
    const start = () => {
      if (timer === undefined) {
        run();
        timer = setInterval(run, intervalMs);
      }
    };
    const stop = () => {
      clearInterval(timer);
      timer = undefined;
      controller?.abort();
    };
    const onVisibilityChange = () => (document.hidden ? stop() : start());
    if (!document.hidden) start();
    document.addEventListener("visibilitychange", onVisibilityChange);
    return () => {
      document.removeEventListener("visibilitychange", onVisibilityChange);
      stop();
    };
  }, [load, intervalMs, enabled]);
}
