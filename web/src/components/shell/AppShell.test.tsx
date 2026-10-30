import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Me } from "@/lib/api";
import { THEME_KEY, THEME_SCRIPT } from "@/lib/theme";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";
import { ACME, LYON, SHELL_ROUTES } from "@/test/shell";

import { AppShell } from "./AppShell";

vi.mock("next/navigation", () => import("@/test/navigation"));

const admin: Me = { id: "u1", username: "alice", roles: [{ role: "administrator", customer_id: null, site_id: null }] };

function renderShell() {
  stubFetch(SHELL_ROUTES);
  return render(<AppShell me={admin}>page</AppShell>);
}

const nav = () => screen.getByRole("navigation", { name: "Main" });

describe("AppShell", () => {
  beforeEach(() => {
    setUrl("/");
    localStorage.clear();
    document.documentElement.removeAttribute("data-theme");
  });

  it("starts with a skip link to the main content", async () => {
    renderShell();
    await userEvent.tab();
    expect(screen.getByRole("link", { name: "Skip to content" })).toHaveFocus();
    expect(screen.getByRole("link", { name: "Skip to content" })).toHaveAttribute("href", "#main");
    expect(screen.getByRole("main")).toHaveAttribute("id", "main");
  });

  it("marks the current page's link, including pages under it", () => {
    setUrl("/incidents/view/?id=x");
    const { unmount } = renderShell();
    expect(within(nav()).getByRole("link", { name: /^Incidents/ })).toHaveAttribute("aria-current", "page");
    expect(within(nav()).getByRole("link", { name: "Fleet" })).not.toHaveAttribute("aria-current");
    unmount();

    setUrl("/servers/view/?id=x");
    renderShell();
    expect(within(nav()).getByRole("link", { name: "Fleet" })).toHaveAttribute("aria-current", "page");
  });

  it("lists the pages of the handoff's navigation", () => {
    renderShell();
    const links = within(nav())
      .getAllByRole("link")
      .map((a) => a.textContent);
    expect(links).toEqual([
      "MonitorFlowMF",
      "Fleet",
      "Incidents",
      "Mirrors",
      "Automatic updates",
      "Campaigns",
      "Approvals",
      "Audit log",
      "Relays",
      "Sites",
      "Add servers",
      "Users and roles",
      "Server update",
      "Backups",
    ]);
    expect(within(nav()).getByText("Remediation")).toBeInTheDocument();
    expect(within(nav()).getByText("Updates")).toBeInTheDocument();
  });

  it("counts open incidents in the Incidents link, within the scope", async () => {
    const { unmount } = renderShell();
    expect(await within(nav()).findByLabelText("3 open incidents")).toHaveTextContent("3");
    unmount();

    setUrl(`/?customer=${ACME}`);
    renderShell();
    expect(await within(nav()).findByLabelText("2 open incidents")).toBeInTheDocument();
  });

  it("stores the chosen customer in the URL and keeps it in every link", async () => {
    const user = userEvent.setup();
    renderShell();
    await user.click(await screen.findByRole("button", { name: "Scope: All customers" }));
    const list = screen.getByRole("listbox", { name: "Scope" });
    expect(within(list).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "All customers",
      "Acme Hosting",
      "paris-dc1",
      "lyon-office",
      "Northwind Logistics",
      "berlin-hq",
    ]);
    await user.click(within(list).getByRole("option", { name: "Acme Hosting" }));

    expect(window.location.search).toBe(`?customer=${ACME}`);
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Scope: Acme Hosting" })).toHaveFocus();
    expect(within(nav()).getByRole("link", { name: "Fleet" })).toHaveAttribute("href", `/?customer=${ACME}`);
    expect(within(nav()).getByRole("link", { name: "Relays" })).toHaveAttribute(
      "href",
      // Next's Link drops the trailing slash outside a build with `trailingSlash`.
      expect.stringMatching(new RegExp(`^/admin/relays/?\\?customer=${ACME}$`)),
    );
  });

  it("picks a site with the keyboard and closes on Escape", async () => {
    const user = userEvent.setup();
    renderShell();
    const button = await screen.findByRole("button", { name: "Scope: All customers" });
    // Wait for the lists, then open with the keyboard.
    await screen.findByLabelText("3 open incidents");
    button.focus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("option", { name: "All customers" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("listbox")).not.toBeInTheDocument();
    expect(button).toHaveFocus();

    await user.keyboard("{ArrowDown}{ArrowDown}{ArrowDown}{ArrowDown}");
    expect(screen.getByRole("option", { name: "lyon-office" })).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(new URLSearchParams(window.location.search).get("customer")).toBe(ACME);
    expect(new URLSearchParams(window.location.search).get("site")).toBe(LYON);
    // The button names the customer only; the site is a filter.
    expect(screen.getByRole("button", { name: "Scope: Acme Hosting" })).toBeInTheDocument();
  });

  it("cycles the theme, stores it and applies it to <html>", async () => {
    const user = userEvent.setup();
    renderShell();
    const root = document.documentElement;
    await user.click(screen.getByRole("button", { name: "Theme: System" }));
    expect(root).toHaveAttribute("data-theme", "light");
    expect(localStorage.getItem(THEME_KEY)).toBe("light");
    await user.click(screen.getByRole("button", { name: "Theme: Light" }));
    expect(root).toHaveAttribute("data-theme", "dark");
    expect(localStorage.getItem(THEME_KEY)).toBe("dark");
    await user.click(screen.getByRole("button", { name: "Theme: Dark" }));
    expect(root).not.toHaveAttribute("data-theme");
    expect(localStorage.getItem(THEME_KEY)).toBeNull();
  });

  it("reads the stored theme, which the head script applies before paint", () => {
    localStorage.setItem(THEME_KEY, "dark");
    new Function(THEME_SCRIPT)();
    expect(document.documentElement).toHaveAttribute("data-theme", "dark");
    renderShell();
    expect(screen.getByRole("button", { name: "Theme: Dark" })).toBeInTheDocument();
  });

  it("opens and closes the phone menu", async () => {
    const user = userEvent.setup();
    renderShell();
    const menu = screen.getByRole("button", { name: "Menu" });
    expect(menu).toHaveAttribute("aria-expanded", "false");
    expect(menu).toHaveAttribute("aria-controls", nav().id);
    await user.click(menu);
    expect(menu).toHaveAttribute("aria-expanded", "true");
    await user.click(menu);
    expect(menu).toHaveAttribute("aria-expanded", "false");
  });
});
