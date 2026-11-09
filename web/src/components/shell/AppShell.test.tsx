import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Me } from "@/lib/api";
import { THEME_KEY, THEME_SCRIPT } from "@/lib/theme";
import { stubFetch } from "@/test/fetch";
import { authority, rotation } from "@/test/certificateAuthority";
import { setUrl } from "@/test/navigation";
import { ACME, COMPANY_ROUTES, LYON, SHELL_ROUTES, sites } from "@/test/shell";

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

  describe("update banner", () => {
    const release = { version: "0.2.0", release_notes_url: null, first_seen_at: "2026-10-27T10:00:00Z" };
    const status = (available: typeof release | null, requested = false) => ({
      "GET /api/server-update": {
        status: 200,
        body: { running_version: "0.1.0", update: null, requested, available },
      },
    });

    it("leads global administrators to the update", async () => {
      stubFetch({ ...SHELL_ROUTES, ...status(release) });
      render(<AppShell me={admin}>page</AppShell>);
      expect(await screen.findByText("MonitorFlow 0.2.0 is available.")).toBeInTheDocument();
      expect(screen.getByRole("link", { name: "Review and update" })).toHaveAttribute("href", "/admin/server-update");
    });

    it("stays away without a newer release, on the update page and during an update", async () => {
      for (const [routes, url] of [
        [status(null), "/"],
        [status(release), "/admin/server-update/"],
        [status(release, true), "/"],
      ] as const) {
        setUrl(url);
        const fetchMock = stubFetch({ ...SHELL_ROUTES, ...routes });
        const { unmount } = render(<AppShell me={admin}>page</AppShell>);
        await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/api/server-update", expect.anything()));
        await screen.findByLabelText("3 open incidents");
        expect(screen.queryByText(/is available/)).not.toBeInTheDocument();
        unmount();
      }
    });

    it("never asks for customer administrators", async () => {
      const customer: Me = { ...admin, roles: [{ role: "administrator", customer_id: ACME, site_id: null }] };
      const fetchMock = stubFetch(SHELL_ROUTES);
      render(<AppShell me={customer}>page</AppShell>);
      await screen.findByRole("navigation", { name: "Main" });
      expect(fetchMock).not.toHaveBeenCalledWith("/api/server-update", expect.anything());
    });
  });

  describe("certificate authority notice", () => {
    const routes = (body: unknown) => ({ ...SHELL_ROUTES, "GET /api/certificate-authority": { status: 200, body } });

    it("leads global administrators to an emergency change", async () => {
      stubFetch(routes(authority({ rotation: rotation({ stage: "switched", emergency: true }) })));
      render(<AppShell me={admin}>page</AppShell>);
      expect(await screen.findByText("An emergency change of the certificate authority is under way.")).toBeInTheDocument();
      expect(screen.getByRole("link", { name: "See the certificate authority" })).toHaveAttribute(
        "href",
        "/admin/certificate-authority",
      );
    });

    it("stays away when all is well, during a planned change and on its page", async () => {
      for (const [body, url] of [
        [authority(), "/"],
        [authority({ rotation: rotation() }), "/"],
        [authority({ expires_soon: true }), "/admin/certificate-authority/"],
      ] as const) {
        setUrl(url);
        const fetchMock = stubFetch(routes(body));
        const { unmount } = render(<AppShell me={admin}>page</AppShell>);
        await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/api/certificate-authority", expect.anything()));
        await screen.findByLabelText("3 open incidents");
        expect(screen.queryByText(/certificate authority/)).not.toBeInTheDocument();
        unmount();
      }
    });
  });

  describe("license notice", () => {
    const view = (state: string, added: string[]) => ({
      available: true,
      state,
      license: {
        id: "l1",
        customer: "Example Corp",
        edition: "enterprise",
        server_instances: 2,
        issued: "2026-01-01",
        expires: "2026-11-30",
      },
      added: added.map((name) => ({ name, admitted_at: "2026-11-01T00:00:00Z", running: true })),
    });
    const routes = (body: unknown) => ({ ...SHELL_ROUTES, "GET /api/license": { status: 200, body } });

    it("tells global administrators once a central server was added", async () => {
      stubFetch(routes(view("expired", ["host2"])));
      render(<AppShell me={admin}>page</AppShell>);
      expect(
        await screen.findByText(
          "The Enterprise license expired on 30 November 2026. The central server already added keeps running, but no other can be added.",
        ),
      ).toBeInTheDocument();
      expect(screen.getByRole("link", { name: "See the license" })).toHaveAttribute("href", expect.stringContaining("/admin/license"));
    });

    it("says nothing to a single central server", async () => {
      const fetchMock = stubFetch(routes(view("expired", [])));
      render(<AppShell me={admin}>page</AppShell>);
      await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/api/license", expect.anything()));
      await screen.findByLabelText("3 open incidents");
      expect(screen.queryByText(/Enterprise license/)).not.toBeInTheDocument();
    });
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
      "Dashboards",
      "Incidents",
      "Silences",
      "On call",
      "Mirrors",
      "Automatic updates",
      "Campaigns",
      "Approvals",
      "Audit log",
      "Settings",
      "Relays",
      "Sites",
      "Add servers",
      "Users and roles",
      "Notifications",
      "Integrations",
      "AI",
      "Server update",
      "Backups",
      "Certificate authority",
      "License",
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

  describe("a single company (ADR-0104)", () => {
    const company: Me = { ...admin, multi_customer: false };
    const customerUser: Me = {
      ...admin,
      multi_customer: false,
      roles: [{ role: "viewer", customer_id: ACME, site_id: null }],
    };

    for (const [who, user] of [
      ["a single company", company],
      ["an MSP's customer", customerUser],
    ] as const) {
      it(`picks among sites, never customers, for ${who}`, async () => {
        const u = userEvent.setup();
        stubFetch(COMPANY_ROUTES);
        render(<AppShell me={user}>page</AppShell>);
        await u.click(await screen.findByRole("button", { name: "Scope: All sites" }));
        const list = screen.getByRole("listbox", { name: "Scope" });
        expect(within(list).getAllByRole("option").map((o) => o.textContent)).toEqual([
          "All sites",
          "paris-dc1",
          "lyon-office",
        ]);
        await u.click(within(list).getByRole("option", { name: "lyon-office" }));
        expect(new URLSearchParams(window.location.search).get("site")).toBe(LYON);
        expect(screen.getByRole("button", { name: "Scope: lyon-office" })).toBeInTheDocument();
        expect(document.body).not.toHaveTextContent(/customer/i);
      });
    }

    it("shows no picker for one site", async () => {
      stubFetch({ ...COMPANY_ROUTES, "GET /api/sites": { status: 200, body: { sites: [sites[0]!] } } });
      render(<AppShell me={company}>page</AppShell>);
      // The count comes after the lists, fetched first.
      await within(nav()).findByLabelText("3 open incidents");
      expect(screen.queryByRole("button", { name: /^Scope/ })).not.toBeInTheDocument();
    });
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
