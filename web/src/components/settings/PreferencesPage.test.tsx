import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LOCALE_KEY } from "@/i18n/locale";
import { LocaleProvider } from "@/i18n/LocaleProvider";
import { SAVE_DELAY, startPreferences, stopPreferences } from "@/lib/preferences";
import { THEME_KEY } from "@/lib/theme";
import { As, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { PreferencesPage } from "./PreferencesPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const URL = "/api/me/preferences";

async function open(server: object = {}, put: { status: number; body?: unknown } = { status: 200, body: {} }) {
  const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: server }, [`PUT ${URL}`]: put });
  render(
    <LocaleProvider>
      <As user={viewer}>
        <PreferencesPage />
      </As>
    </LocaleProvider>,
  );
  act(() => void startPreferences(viewer.id));
  await act(async () => {
    await vi.runOnlyPendingTimersAsync();
  });
  return fetchMock;
}

async function lastPut(fetchMock: ReturnType<typeof stubFetch>) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(SAVE_DELAY);
  });
  const puts = fetchMock.mock.calls.filter(([, init]) => init?.method === "PUT");
  return JSON.parse(String(puts.at(-1)?.[1]?.body)) as unknown;
}

describe("PreferencesPage", () => {
  beforeEach(() => {
    setUrl("/settings/preferences/");
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    stopPreferences();
    vi.useRealTimers();
  });

  it("shows the stored theme and saves another one with the account", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const fetchMock = await open({ theme: "dark" });
    expect(screen.getByRole("heading", { level: 1, name: "Preferences" })).toBeInTheDocument();
    expect(screen.getByRole("radio", { name: "Dark" })).toBeChecked();
    await user.click(screen.getByRole("radio", { name: "Light" }));
    expect(document.documentElement.getAttribute("data-theme")).toBe("light");
    expect(localStorage.getItem(THEME_KEY)).toBe("light");
    expect(await lastPut(fetchMock)).toEqual({ theme: "light" });
    await user.click(screen.getByRole("radio", { name: "System" }));
    expect(document.documentElement.hasAttribute("data-theme")).toBe(false);
    expect(await lastPut(fetchMock)).toEqual({ theme: "system" });
  });

  it("changes the language and saves it", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const fetchMock = await open();
    await user.selectOptions(screen.getByRole("combobox", { name: "Language" }), "fr");
    expect(localStorage.getItem(LOCALE_KEY)).toBe("fr");
    expect(await lastPut(fetchMock)).toEqual({ language: "fr" });
    expect(await screen.findByRole("heading", { level: 1, name: "Préférences" })).toBeInTheDocument();
  });

  it("holds the sidebar editor, for every role", async () => {
    await open();
    expect(screen.getByRole("heading", { level: 2, name: "Sidebar" })).toHaveAttribute("id", "sidebar");
    expect(screen.getByRole("checkbox", { name: "Fleet" })).toBeChecked();
    expect(screen.getByRole("button", { name: "Reset to default" })).toBeDisabled();
  });

  it("says when a change could not be saved", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    await open({}, { status: 500, body: { error: "down" } });
    await user.click(screen.getByRole("radio", { name: "Dark" }));
    await act(async () => {
      await vi.advanceTimersByTimeAsync(SAVE_DELAY);
    });
    expect(await screen.findByText(/could not be saved/)).toBeInTheDocument();
  });
});
