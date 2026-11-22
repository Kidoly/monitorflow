import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Me, SidebarPreferences } from "@/lib/api";
import { SAVE_DELAY, sidebarKey, startPreferences, stopPreferences } from "@/lib/preferences";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";
import { SHELL_ROUTES } from "@/test/shell";

import { AppShell } from "./AppShell";
import { SidebarEditor } from "./SidebarEditor";

vi.mock("next/navigation", () => import("@/test/navigation"));

const admin: Me = { id: "u1", username: "alice", roles: [{ role: "administrator", customer_id: null, site_id: null }] };
const viewer: Me = { id: "u1", username: "bob", roles: [{ role: "viewer", customer_id: null, site_id: "s1" }] };

const nav = () => screen.getByRole("navigation", { name: "Main" });
/** The names of the sidebar's links, in order, the brand and the person's button left out. */
const links = () =>
  within(nav())
    .getAllByRole("link")
    .filter((link) => !link.getAttribute("href")?.startsWith("/settings/preferences"))
    .map((link) => link.textContent?.replace(/\d+$/, "") ?? "")
    .filter((name) => name !== "MonitorFlow");

/** Renders `ui` with `sidebar` as the stored preferences, loaded. */
async function withPreferences(ui: React.ReactElement, sidebar?: SidebarPreferences) {
  const fetchMock = stubFetch({
    ...SHELL_ROUTES,
    "GET /api/me/preferences": { status: 200, body: sidebar ? { sidebar } : {} },
    "PUT /api/me/preferences": { status: 200, body: {} },
  });
  const view = render(ui);
  act(() => void startPreferences("u1"));
  await act(async () => {
    await vi.runOnlyPendingTimersAsync();
  });
  return { ...view, fetchMock };
}

/** The sidebar of the last PUT sent, after the save delay. */
async function saved(fetchMock: ReturnType<typeof stubFetch>) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(SAVE_DELAY);
  });
  const puts = fetchMock.mock.calls.filter(([, init]) => init?.method === "PUT");
  const body = JSON.parse(String(puts.at(-1)?.[1]?.body ?? "{}")) as { sidebar?: SidebarPreferences };
  return body.sidebar;
}

describe("sidebar preferences", () => {
  beforeEach(() => {
    setUrl("/");
    localStorage.clear();
    vi.useFakeTimers({ shouldAdvanceTime: true });
  });

  afterEach(() => {
    stopPreferences();
    vi.useRealTimers();
  });

  describe("in the shell", () => {
    it("collapses to icons and expands again from the rail, and saves it", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      const { fetchMock } = await withPreferences(<AppShell me={admin}>page</AppShell>);
      // Wide by default in tests: labels.
      expect(nav().className).not.toMatch(/rail/);
      await user.click(screen.getByRole("button", { name: "Collapse sidebar" }));
      expect(nav().className).toMatch(/rail/);
      // The labels stay the links' accessible names.
      expect(screen.getByRole("link", { name: "Fleet" })).toBeInTheDocument();
      expect(await saved(fetchMock)).toEqual({ collapsed: true });
      expect(JSON.parse(localStorage.getItem(sidebarKey("u1"))!)).toEqual({ collapsed: true });
      await user.click(screen.getByRole("button", { name: "Expand sidebar" }));
      expect(nav().className).not.toMatch(/rail/);
      expect(await saved(fetchMock)).toEqual({ collapsed: false });
    });

    it("shows the cached choice before the server answers", () => {
      localStorage.setItem(sidebarKey("u1"), JSON.stringify({ collapsed: true, hidden: ["audit"] }));
      stubFetch({ ...SHELL_ROUTES, "GET /api/me/preferences": { status: 500, body: { error: "down" } } });
      render(<AppShell me={admin}>page</AppShell>);
      act(() => void startPreferences("u1"));
      expect(nav().className).toMatch(/rail/);
      expect(within(nav()).queryByRole("link", { name: "Audit log" })).not.toBeInTheDocument();
    });

    it("hides and reorders items as stored, Settings always at the bottom", async () => {
      await withPreferences(<AppShell me={admin}>page</AppShell>, {
        hidden: ["silences", "mirrors", "audit"],
        order: ["incidents", "fleet", "campaigns", "mirrors"],
      });
      expect(links()).toEqual([
        "Incidents",
        "Dashboards",
        "Fleet",
        "On call",
        "Reports",
        "Campaigns",
        "Automatic updates",
        "Approvals",
        "Settings",
      ]);
    });

    it("never shows what the role hides, whatever the preferences say", async () => {
      await withPreferences(<AppShell me={viewer}>page</AppShell>, { order: ["mirrors", "campaigns", "fleet"] });
      const names = links();
      expect(names).not.toContain("Linux mirrors");
      expect(names).not.toContain("Campaigns");
      expect(names[0]).toBe("Fleet");
      // Settings always shows: it leads to the person's preferences.
      expect(names.at(-1)).toBe("Settings");
    });
  });

  describe("editor", () => {
    async function renderEditor(me: Me, sidebar?: SidebarPreferences) {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      const view = await withPreferences(
        <AppShell me={me}>
          <SidebarEditor me={me} />
        </AppShell>,
        sidebar,
      );
      return { ...view, user, main: within(screen.getByRole("main")) };
    }

    it("hides an item and shows it again", async () => {
      const { user, main, fetchMock } = await renderEditor(admin);
      await user.click(main.getByRole("switch", { name: "Audit log" }));
      expect(within(nav()).queryByRole("link", { name: "Audit log" })).not.toBeInTheDocument();
      expect(await saved(fetchMock)).toEqual({ hidden: ["audit"] });
      await user.click(main.getByRole("switch", { name: "Audit log" }));
      expect(within(nav()).getByRole("link", { name: "Audit log" })).toBeInTheDocument();
    });

    it("never lets Settings be hidden", async () => {
      const { main } = await renderEditor(admin);
      const settings = main.getByRole("switch", { name: "Settings" });
      expect(settings).toBeChecked();
      expect(settings).toBeDisabled();
      expect(settings).toHaveAccessibleDescription("Always shown");
    });

    it("reorders from the keyboard, keeps focus and says where the item went", async () => {
      const { user, main, fetchMock } = await renderEditor(admin);
      const down = main.getByRole("button", { name: "Move Fleet down" });
      expect(main.getByRole("button", { name: "Move Fleet up" })).toBeDisabled();
      down.focus();
      await user.keyboard("{Enter}");
      expect(links().slice(0, 3)).toEqual(["Dashboards", "Fleet", "Incidents"]);
      expect(main.getByRole("button", { name: "Move Fleet down" })).toHaveFocus();
      expect(screen.getByRole("status")).toHaveTextContent("Fleet moved to position 2 of 5.");
      const sidebar = await saved(fetchMock);
      expect(sidebar?.order?.slice(0, 3)).toEqual(["dashboards", "fleet", "incidents"]);
      // At the top, focus moves to the button that still works.
      await user.click(main.getByRole("button", { name: "Move Fleet up" }));
      expect(main.getByRole("button", { name: "Move Fleet up" })).toBeDisabled();
      expect(main.getByRole("button", { name: "Move Fleet down" })).toHaveFocus();
    });

    it("moves items within their section only", async () => {
      const { main } = await renderEditor(admin);
      expect(main.getByRole("button", { name: "Move Reports down" })).toBeDisabled();
      expect(main.getByRole("button", { name: "Move Linux mirrors up" })).toBeDisabled();
    });

    it("picks labels, icons only or automatic", async () => {
      const { user, main, fetchMock } = await renderEditor(admin);
      expect(main.getByRole("radio", { name: /Automatic/ })).toBeChecked();
      await user.click(main.getByRole("radio", { name: "Icons only" }));
      expect(nav().className).toMatch(/rail/);
      expect(await saved(fetchMock)).toEqual({ collapsed: true });
      await user.click(main.getByRole("radio", { name: /Automatic/ }));
      expect(nav().className).not.toMatch(/rail/);
    });

    it("resets to the default sidebar", async () => {
      const { user, main, fetchMock } = await renderEditor(admin, { collapsed: true, hidden: ["fleet"], order: ["audit"] });
      expect(within(nav()).queryByRole("link", { name: "Fleet" })).not.toBeInTheDocument();
      await user.click(main.getByRole("button", { name: "Reset to default" }));
      expect(within(nav()).getByRole("link", { name: "Fleet" })).toBeInTheDocument();
      expect(nav().className).not.toMatch(/rail/);
      expect(main.getByRole("button", { name: "Reset to default" })).toBeDisabled();
      expect(await saved(fetchMock)).toBeUndefined();
      expect(localStorage.getItem(sidebarKey("u1"))).toBeNull();
    });

    it("lists only what the role shows", async () => {
      const { main } = await renderEditor(viewer);
      expect(main.queryByRole("switch", { name: "Linux mirrors" })).not.toBeInTheDocument();
      expect(main.queryByRole("switch", { name: "Campaigns" })).not.toBeInTheDocument();
      expect(main.getByRole("switch", { name: "Fleet" })).toBeChecked();
    });
  });
});
