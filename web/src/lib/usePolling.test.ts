import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { usePolling } from "./usePolling";

function setHidden(hidden: boolean) {
  Object.defineProperty(document, "hidden", { configurable: true, get: () => hidden });
  document.dispatchEvent(new Event("visibilitychange"));
}

describe("usePolling", () => {
  afterEach(() => {
    setHidden(false);
    vi.useRealTimers();
  });

  it("loads at once, then on every interval", async () => {
    vi.useFakeTimers();
    const load = vi.fn(async () => {});
    renderHook(() => usePolling(load, 1000));
    expect(load).toHaveBeenCalledTimes(1);
    await act(() => vi.advanceTimersByTimeAsync(2000));
    expect(load).toHaveBeenCalledTimes(3);
  });

  it("pauses while the tab is hidden and aborts the request in flight", async () => {
    vi.useFakeTimers();
    const signals: AbortSignal[] = [];
    const load = vi.fn(async (signal: AbortSignal) => void signals.push(signal));
    renderHook(() => usePolling(load, 1000));
    act(() => setHidden(true));
    expect(signals[0]?.aborted).toBe(true);
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(load).toHaveBeenCalledTimes(1);

    act(() => setHidden(false));
    expect(load).toHaveBeenCalledTimes(2);
  });

  it("does nothing when disabled and stops on unmount", async () => {
    vi.useFakeTimers();
    const load = vi.fn(async () => {});
    const { rerender, unmount } = renderHook(({ enabled }) => usePolling(load, 1000, enabled), {
      initialProps: { enabled: false },
    });
    expect(load).not.toHaveBeenCalled();
    rerender({ enabled: true });
    expect(load).toHaveBeenCalledTimes(1);
    unmount();
    await act(() => vi.advanceTimersByTimeAsync(5000));
    expect(load).toHaveBeenCalledTimes(1);
  });
});
