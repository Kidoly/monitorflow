import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LOCALE_KEY } from "@/i18n/locale";
import { LocaleProvider } from "@/i18n/LocaleProvider";
import { SAVE_DELAY, startPreferences, stopPreferences } from "@/lib/preferences";
import { THEME_KEY } from "@/lib/theme";
import { As, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { router, setUrl } from "@/test/navigation";

import { PreferencesPage } from "./PreferencesPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const URL = "/api/me/preferences";

type Reply = { status: number; body?: unknown };

async function open(server: object = {}, put: Reply = { status: 200, body: {} }, more: Record<string, Reply | Reply[]> = {}) {
  const fetchMock = stubFetch({ [`GET ${URL}`]: { status: 200, body: server }, [`PUT ${URL}`]: put, ...more });
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
    router.replace.mockClear();
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

  it("has Language, Appearance, Sidebar and Password sections, for every role", async () => {
    await open();
    expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
      "Language",
      "Appearance",
      "Sidebar",
      "Password",
    ]);
    expect(screen.getByText("Signed in as alice (viewer)")).toBeInTheDocument();
  });

  it("signs out from the header", async () => {
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const fetchMock = await open({}, undefined, { "POST /api/auth/logout": { status: 204 } });
    await user.click(screen.getByRole("button", { name: "Sign out" }));
    expect(fetchMock).toHaveBeenCalledWith("/api/auth/logout", expect.objectContaining({ method: "POST" }));
    expect(router.replace).toHaveBeenCalledWith("/login/");
  });

  describe("password", () => {
    const PASSWORD_URL = "POST /api/me/password";

    async function fill(user: ReturnType<typeof userEvent.setup>, current: string, next: string, confirm = next) {
      await user.type(screen.getByLabelText("Current password"), current);
      await user.type(screen.getByLabelText("New password"), next);
      await user.type(screen.getByLabelText("Confirm password"), confirm);
      await user.click(screen.getByRole("button", { name: "Change password" }));
    }

    it("changes it with the current one and says the other sessions ended", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      const fetchMock = await open({}, undefined, { [PASSWORD_URL]: { status: 204 } });
      await fill(user, "old password here", "a new long password");
      expect(await screen.findByText("Password changed. Your other sessions were signed out.")).toBeInTheDocument();
      const call = fetchMock.mock.calls.find(([url]) => url === "/api/me/password");
      expect(JSON.parse(String(call?.[1]?.body))).toEqual({
        current_password: "old password here",
        new_password: "a new long password",
      });
      expect(screen.getByLabelText("Current password")).toHaveValue("");
    });

    it("refuses two different new passwords without asking the server", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      const fetchMock = await open();
      await fill(user, "old password here", "a new long password", "another long password");
      expect(screen.getByRole("alert")).toHaveTextContent("The passwords differ.");
      expect(screen.getByLabelText("Confirm password")).toHaveAttribute("aria-invalid", "true");
      expect(fetchMock.mock.calls.some(([url]) => url === "/api/me/password")).toBe(false);
    });

    it("shows the server's reason, like a wrong current password", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      await open({}, undefined, {
        [PASSWORD_URL]: { status: 400, body: { error: "The current password is not right.", code: "invalid" } },
      });
      await fill(user, "wrong password!!", "a new long password");
      expect(await screen.findByRole("alert")).toHaveTextContent("The current password is not right.");
      // Still signed in: a 400 is not taken for an expired session.
      expect(router.replace).not.toHaveBeenCalledWith("/login/");
    });

    it("asks for at least 12 characters", async () => {
      await open();
      const field = screen.getByLabelText("New password");
      expect(field).toHaveAttribute("minLength", "12");
      expect(field).toHaveAccessibleDescription(/At least 12 characters/);
    });
  });
});
