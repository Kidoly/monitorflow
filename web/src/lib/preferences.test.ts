import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LOCALE_KEY } from "@/i18n/locale";
import { stubFetch } from "@/test/fetch";

import { THEME_KEY } from "./theme";
import {
  SAVE_DELAY,
  browserPreferences,
  setLanguage,
  setPreference,
  setTheme,
  sidebarKey,
  startPreferences,
  stopPreferences,
  usePreferences,
} from "./preferences";

const URL = "/api/me/preferences";

/** The bodies of the PUTs sent so far. */
function puts(fetchMock: ReturnType<typeof stubFetch>) {
  return fetchMock.mock.calls
    .filter(([, init]) => init?.method === "PUT")
    .map(([, init]) => JSON.parse(String(init?.body)) as unknown);
}

async function settle() {
  await act(async () => {
    await vi.runOnlyPendingTimersAsync();
  });
}

describe("preferences", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    vi.useFakeTimers();
  });

  afterEach(() => {
    stopPreferences();
    vi.useRealTimers();
  });

  it("reads what this browser holds before the server answers", () => {
    localStorage.setItem(THEME_KEY, "dark");
    localStorage.setItem(LOCALE_KEY, "fr");
    localStorage.setItem(sidebarKey("u1"), JSON.stringify({ collapsed: true }));
    expect(browserPreferences("u1")).toEqual({ theme: "dark", language: "fr", sidebar: { collapsed: true } });
    // Another person's sidebar on this browser is not theirs.
    expect(browserPreferences("u2")).toEqual({ theme: "dark", language: "fr" });
    localStorage.setItem(sidebarKey("u1"), "{not json");
    expect(browserPreferences("u1").sidebar).toBeUndefined();
  });

  it("moves the browser's theme and language to the server the first time", async () => {
    localStorage.setItem(THEME_KEY, "dark");
    localStorage.setItem(LOCALE_KEY, "fr");
    const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: {} }, [`PUT ${URL}`]: { status: 200, body: {} } });
    startPreferences("u1");
    await settle();
    expect(puts(fetchMock)).toEqual([{ theme: "dark", language: "fr" }]);
    // The browser keeps them as its first-paint cache.
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");
    expect(localStorage.getItem(LOCALE_KEY)).toBe("fr");
  });

  it("sends nothing when the browser has nothing the server lacks", async () => {
    const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: { theme: "light" } } });
    startPreferences("u1");
    await settle();
    expect(puts(fetchMock)).toEqual([]);
  });

  it("lets the server win and refreshes the browser's cache with it", async () => {
    localStorage.setItem(THEME_KEY, "dark");
    localStorage.setItem(LOCALE_KEY, "en");
    const server = { theme: "light", language: "fr", sidebar: { hidden: ["audit"] } };
    const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: server } });
    const { result } = renderHook(() => usePreferences());
    act(() => void startPreferences("u1"));
    await settle();
    expect(puts(fetchMock)).toEqual([]);
    expect(result.current.loaded).toBe(true);
    expect(result.current.preferences).toEqual(server);
    expect(localStorage.getItem(THEME_KEY)).toBe("light");
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(localStorage.getItem(LOCALE_KEY)).toBe("fr");
    expect(JSON.parse(localStorage.getItem(sidebarKey("u1"))!)).toEqual({ hidden: ["audit"] });
  });

  it("keeps an explicit system theme over a browser's older choice", async () => {
    localStorage.setItem(THEME_KEY, "dark");
    const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: { theme: "system" } } });
    startPreferences("u1");
    await settle();
    expect(puts(fetchMock)).toEqual([]);
    expect(localStorage.getItem(THEME_KEY)).toBeNull();
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
  });

  it("applies a change at once and saves a burst of them once", async () => {
    const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: {} }, [`PUT ${URL}`]: { status: 200, body: {} } });
    startPreferences("u1");
    await settle();
    setTheme("dark");
    expect(document.documentElement.getAttribute("data-theme")).toBe("dark");
    setPreference("sidebar", { collapsed: true });
    expect(JSON.parse(localStorage.getItem(sidebarKey("u1"))!)).toEqual({ collapsed: true });
    setLanguage("fr");
    expect(puts(fetchMock)).toEqual([]);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SAVE_DELAY);
    });
    expect(puts(fetchMock)).toEqual([{ theme: "dark", sidebar: { collapsed: true }, language: "fr" }]);
  });

  it("keeps a change made before the server answered", async () => {
    let answer!: (value: Response) => void;
    const fetchMock = stubFetch({ [`PUT ${URL}`]: { status: 200, body: {} } });
    const stubbed = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation((input, init) =>
      init?.method === "PUT" ? stubbed(input, init) : new Promise<Response>((resolve) => (answer = resolve)),
    );
    startPreferences("u1");
    setPreference("sidebar", { collapsed: true });
    answer(new Response(JSON.stringify({ theme: "light", sidebar: { hidden: ["audit"] } }), { status: 200 }));
    await settle();
    expect(puts(fetchMock)).toEqual([{ theme: "light", sidebar: { collapsed: true } }]);
  });

  it("never writes over the server's document when it could not read it", async () => {
    localStorage.setItem(THEME_KEY, "dark");
    const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 500, body: { error: "down" } } });
    startPreferences("u1");
    await settle();
    setTheme("light");
    await settle();
    expect(puts(fetchMock)).toEqual([]);
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
  });

  it("says when a save failed", async () => {
    stubFetch({ [`GET ${URL}`]: { status: 200, body: {} }, [`PUT ${URL}`]: { status: 500, body: { error: "down" } } });
    const { result } = renderHook(() => usePreferences());
    act(() => void startPreferences("u1"));
    await settle();
    act(() => setTheme("dark"));
    await settle();
    expect(result.current.save).toBe("error");
  });

  it("only applies a choice when nobody is signed in", () => {
    const fetchMock = stubFetch({});
    setTheme("dark");
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
