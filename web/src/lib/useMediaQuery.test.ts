import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { useMediaQuery } from "./useMediaQuery";

describe("useMediaQuery", () => {
  it("falls back without matchMedia", () => {
    expect(renderHook(() => useMediaQuery("(min-width: 800px)", true)).result.current).toBe(true);
    expect(renderHook(() => useMediaQuery("(min-width: 800px)", false)).result.current).toBe(false);
  });

  it("follows the query", () => {
    let matches = false;
    const listeners = new Set<() => void>();
    vi.stubGlobal("matchMedia", (query: string) => ({
      media: query,
      get matches() {
        return matches;
      },
      addEventListener: (_: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_: string, listener: () => void) => listeners.delete(listener),
    }));
    const { result } = renderHook(() => useMediaQuery("(max-width: 1099px)", true));
    expect(result.current).toBe(false);
    act(() => {
      matches = true;
      listeners.forEach((listener) => listener());
    });
    expect(result.current).toBe(true);
  });
});
