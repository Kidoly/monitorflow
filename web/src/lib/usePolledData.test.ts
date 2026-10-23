import { act, renderHook } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { REFRESH_MS, usePolledData } from "./usePolledData";

describe("usePolledData", () => {
  afterEach(() => vi.useRealTimers());

  it("refreshes every 15 s, keeps the last payload and turns stale on failure", async () => {
    vi.useFakeTimers();
    const fetcher = vi
      .fn<(signal: AbortSignal) => Promise<number>>()
      .mockResolvedValueOnce(1)
      .mockRejectedValueOnce(new Error("timeout"))
      .mockRejectedValueOnce(new Error("timeout"))
      .mockResolvedValueOnce(2);
    const { result } = renderHook(() => usePolledData(fetcher));
    await act(() => vi.advanceTimersByTimeAsync(0));
    expect(result.current).toMatchObject({ data: 1, stale: false, failures: 0 });
    const firstUpdate = result.current.updatedAt;

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(result.current).toMatchObject({ data: 1, stale: true, failures: 1 });
    expect(result.current.updatedAt).toBe(firstUpdate);
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(result.current).toMatchObject({ data: 1, stale: true, failures: 2 });

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(result.current).toMatchObject({ data: 2, stale: false, failures: 0, error: null });
    expect(fetcher).toHaveBeenCalledTimes(4);
  });

  it("is an error, not stale, when the first load fails", async () => {
    // Stable, as the hook requires: a new fetcher on each render would refetch each render.
    const fetcher = () => Promise.reject(new Error("502"));
    const { result } = renderHook(() => usePolledData(fetcher));
    await vi.waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    expect(result.current).toMatchObject({ data: undefined, stale: false, failures: 1 });
  });

  it("fetches again at once on refresh", async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn(async () => "ok");
    const { result } = renderHook(() => usePolledData(fetcher));
    await act(() => vi.advanceTimersByTimeAsync(0));
    act(() => result.current.refresh());
    await act(() => vi.advanceTimersByTimeAsync(0));
    expect(fetcher).toHaveBeenCalledTimes(2);
  });
});
