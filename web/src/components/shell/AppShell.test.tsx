import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Me } from "@/lib/api";
import { setActiveLocale } from "@/i18n/locale";
import { THEME_KEY, THEME_SCRIPT } from "@/lib/theme";
import { stubFetch } from "@/test/fetch";
import { authority, rotation } from "@/test/certificateAuthority";
import { setUrl } from "@/test/navigation";
import { ACME, COMPANY_ROUTES, LYON, SHELL_ROUTES, sites } from "@/test/shell";

import { SettingsStatusProvider } from "../settings/SettingsStatus";

import { AppShell, initials } from "./AppShell";

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
      expect(screen.getByRole("link", { name: "Review and update" })).toHaveAttribute("href", "/settings/server-update");
    });

    it("stays away without a newer release, on the update page and during an update", async () => {
      for (const [routes, url] of [
        [status(null), "/"],
        [status(release), "/settings/server-update/"],
        [status(release, true), "/"],
      ] as const) {
        setUrl(url);
        const fetchMock = stubFetch({ ...SHELL_ROUTES, ...routes });
        const { unmount } = render(<AppShell me={admin}>page</AppShell>);
        await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/api/server-update", expect.anything()));
        await screen.findByText("3 open");
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
        "/settings/certificate-authority",
      );
    });

    it("stays away when all is well, during a planned change and on its page", async () => {
      for (const [body, url] of [
        [authority(), "/"],
        [authority({ rotation: rotation() }), "/"],
        [authority({ expires_soon: true }), "/settings/certificate-authority/"],
      ] as const) {
        setUrl(url);
        const fetchMock = stubFetch(routes(body));
        const { unmount } = render(<AppShell me={admin}>page</AppShell>);
        await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/api/certificate-authority", expect.anything()));
        await screen.findByText("3 open");
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
      expect(screen.getByRole("link", { name: "See the license" })).toHaveAttribute("href", expect.stringContaining("/settings/license"));
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
      "MonitorFlow",
      "Fleet",
      "Dashboards",
      "Network maps",
      "Incidents",
      "On call",
      "Reports",
      "Linux mirrors",
      "Automatic updates",
      "Campaigns",
      "Approvals",
      "Audit log",
      "Scripts",
      "Automation",
      "Settings",
      "Aalice",
    ]);
    expect(within(nav()).queryByText("Administration")).not.toBeInTheDocument();
    expect(within(nav()).getByText("Remediation")).toBeInTheDocument();
    expect(within(nav()).getByText("Updates")).toBeInTheDocument();
  });

  it("speaks French when that is the language", async () => {
    setActiveLocale("fr");
    renderShell();
    const links = within(nav())
      .getAllByRole("link")
      .map((a) => a.textContent);
    expect(links.slice(1, 7)).toEqual(["Parc", "Tableaux de bord", "Cartes réseau", "Incidents", "Astreinte", "Rapports"]);
    expect(within(nav()).getByText("Supervision")).toBeInTheDocument();
    expect(await within(nav()).findByLabelText("3 incidents ouverts")).toHaveTextContent("3");
    expect(screen.getByRole("link", { name: "Aller au contenu" })).toBeInTheDocument();
    expect(within(nav()).getByRole("link", { name: "alice" })).toHaveAttribute(
      "title",
      "alice (administrateur)\u00a0: préférences et déconnexion",
    );
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
    expect(within(nav()).getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      // Next's Link drops the trailing slash outside a build with `trailingSlash`.
      expect.stringMatching(new RegExp(`^/settings/?\\?customer=${ACME}$`)),
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

  describe("foot", () => {
    it("is one button with the person's name, leading to Preferences", async () => {
      renderShell();
      const button = within(nav()).getByRole("link", { name: "alice" });
      expect(button).toHaveAttribute("href", expect.stringMatching(/^\/settings\/preferences\/?$/));
      expect(button).toHaveTextContent(/^Aalice$/);
      // The role is in the tooltip, not under the name.
      expect(button).toHaveAttribute("title", "alice (administrator): preferences and sign out");
      expect(within(nav()).queryByText(/administrator/)).not.toBeInTheDocument();
    });

    it("leaves theme, language and sign out to Preferences", () => {
      renderShell();
      expect(within(nav()).queryByRole("combobox")).not.toBeInTheDocument();
      expect(within(nav()).queryByRole("button", { name: /theme/i })).not.toBeInTheDocument();
      expect(within(nav()).queryByRole("button", { name: "Sign out" })).not.toBeInTheDocument();
      expect(within(nav()).queryByRole("link", { name: /personalize/i })).not.toBeInTheDocument();
      // Below the scope switcher, only the collapse toggle stays a button.
      expect(within(nav()).getAllByRole("button").map((b) => b.getAttribute("aria-label") ?? b.textContent)).toEqual([
        "Scope: All customers",
        "Collapse sidebar",
      ]);
    });

    it("marks the button current on Preferences", () => {
      setUrl("/settings/preferences/");
      renderShell();
      expect(within(nav()).getByRole("link", { name: "alice" })).toHaveAttribute("aria-current", "page");
    });

    it("draws initials from the name's first two parts", () => {
      expect(initials("lea.martin")).toBe("LM");
      expect(initials("tom_reyes")).toBe("TR");
      expect(initials("admin")).toBe("A");
      expect(initials("Jean Dupont")).toBe("JD");
      expect(initials("élodie")).toBe("É");
      expect(initials("ops@acme")).toBe("OA");
    });
  });

  it("reads the stored theme, which the head script applies before paint", () => {
    localStorage.setItem(THEME_KEY, "dark");
    new Function(THEME_SCRIPT)();
    expect(document.documentElement).toHaveAttribute("data-theme", "dark");
  });

  it("follows a theme chosen in another tab", () => {
    renderShell();
    localStorage.setItem(THEME_KEY, "dark");
    act(() => void window.dispatchEvent(new StorageEvent("storage", { key: THEME_KEY })));
    expect(document.documentElement).toHaveAttribute("data-theme", "dark");
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

  describe("settings", () => {
    const failedBackup = {
      "GET /api/backups": {
        status: 200,
        body: { enabled: true, last: { state: "failed" }, last_success_at: null, files: [] },
      },
    };

    function renderWithStatuses(routes = {}) {
      const fetchMock = stubFetch({ ...SHELL_ROUTES, ...routes });
      render(
        <SettingsStatusProvider me={admin}>
          <AppShell me={admin}>page</AppShell>
        </SettingsStatusProvider>,
      );
      return fetchMock;
    }

    it("pins Settings last, badged when a settings page needs someone", async () => {
      renderWithStatuses(failedBackup);
      const settings = within(nav()).getByRole("link", { name: /^Settings/ });
      expect(await within(settings).findByLabelText("1 setting needs attention")).toHaveTextContent("1");
    });

    it("shows no badge when every page is fine", async () => {
      const fetchMock = renderWithStatuses();
      await vi.waitFor(() => expect(fetchMock).toHaveBeenCalledWith("/api/backups", expect.anything()));
      expect(within(nav()).queryByLabelText(/need.? attention/)).not.toBeInTheDocument();
    });

    it("swaps the sidebar for the settings list inside a settings page, Back leading to the last app page", async () => {
      const user = userEvent.setup();
      setUrl("/incidents/?state=open");
      renderWithStatuses(failedBackup);
      act(() => setUrl("/settings/relays/view/?id=r1"));
      expect(within(nav()).queryByRole("link", { name: "Fleet" })).not.toBeInTheDocument();
      expect(within(nav()).getByRole("link", { name: "Back" })).toHaveAttribute(
        "href",
        expect.stringMatching(/^\/incidents\/?\?state=open$/),
      );
      expect(within(nav()).getByRole("link", { name: "Relays" })).toHaveAttribute("aria-current", "page");
      expect(await within(nav()).findByRole("link", { name: /^Backups\s*Critical$/ })).toBeInTheDocument();
      // The list filters like the Settings page, and Escape clears it.
      const search = within(nav()).getByRole("searchbox", { name: "Search settings" });
      await user.type(search, "key");
      const names = () => within(nav()).getAllByRole("link").map((a) => a.textContent);
      expect(names()).toEqual([
        "MonitorFlow",
        "Back",
        "Settings",
        "Sign-in",
        "API keys",
        "AI",
        "License",
        "Aalice",
      ]);
      await user.keyboard("{Escape}");
      expect(search).toHaveValue("");
      expect(names()).toContain("General");
    });

    it("keeps the app sidebar on the Settings page itself", () => {
      setUrl("/settings/");
      renderShell();
      expect(within(nav()).getByRole("link", { name: "Fleet" })).toBeInTheDocument();
      expect(within(nav()).getByRole("link", { name: "Settings" })).toHaveAttribute("aria-current", "page");
    });
  });
});
