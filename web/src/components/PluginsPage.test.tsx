import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Customer, InstalledPlugin, PluginInspection, Site } from "@/lib/api";
import { As, customerAdmin, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { PluginsPage, placeChoices, placesSummary } from "./PluginsPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });

function plugin(overrides: Partial<InstalledPlugin> = {}): InstalledPlugin {
  return {
    id: "p1",
    name: "ntfy",
    kind: "channel",
    version: "1.0.0",
    title: "ntfy",
    description: "Sends notifications to an ntfy topic.",
    official: false,
    publisher: "Acme IT",
    fingerprint: "0123456789abcdef",
    capabilities: ["Sends notifications from the central server", "Connects to the address in its Server setting"],
    settings: [],
    enabled: true,
    places: [{ customer_id: null, customer_name: null, site_id: null, site_name: null }],
    channels: 2,
    checks: 0,
    installed_at: "2026-11-07T10:00:00Z",
    updated_at: "2026-11-07T10:00:00Z",
    ...overrides,
  };
}

function inspection(overrides: Partial<PluginInspection> = {}): PluginInspection {
  return {
    name: "http-json",
    kind: "check",
    version: "1.0.0",
    title: "Web address",
    description: "Fetches a web address.",
    official: false,
    publisher: "Acme IT",
    fingerprint: "fedcba9876543210",
    capabilities: ["Runs on your servers as a check", "Connects to the address in its Address setting"],
    settings: [],
    sha256: "ab".repeat(32),
    update: null,
    ...overrides,
  };
}

const customer = (id: string, name: string): Customer => ({ id, name, site_count: 1 });
const site = (id: string, name: string, customer_id: string, customer_name: string) =>
  ({ id, name, customer_id, customer_name, server_count: 1, relay_count: 0 }) as Site;

function routes(plugins: InstalledPlugin[], extra: Record<string, unknown> = {}) {
  return {
    "GET /api/plugins": { status: 200, body: { plugins } },
    "GET /api/plugins/publishers": { status: 200, body: { publishers: [], official: [] } },
    "GET /api/customers": { status: 200, body: { customers: [customer("c1", "acme")] } },
    "GET /api/sites": { status: 200, body: { sites: [site("s1", "main", "c1", "acme")] } },
    ...extra,
  } as Parameters<typeof stubFetch>[0];
}

function renderPage(user = globalAdmin) {
  return render(
    <As user={user}>
      <PluginsPage />
    </As>,
  );
}

describe("PluginsPage", () => {
  beforeEach(() => setUrl("/admin/plugins/"));

  it("is for global administrators only", () => {
    stubFetch({});
    renderPage(customerAdmin);
    expect(screen.getByText("Plugins need a global administrator")).toBeInTheDocument();
  });

  it("lists installed plugins with what they ask for and where they run", async () => {
    stubFetch(routes([plugin()]));
    renderPage();
    const table = await screen.findByRole("table", { name: "Installed plugins" });
    expect(within(table).getByText(/signed by Acme IT/)).toBeInTheDocument();
    expect(within(table).getByText("Connects to the address in its Server setting")).toBeInTheDocument();
    expect(within(table).getByText("Everywhere")).toBeInTheDocument();
    expect(within(table).getByText("2 channel(s)")).toBeInTheDocument();
  });

  it("shows what a file asks for before installing it, then asks where to use it", async () => {
    const user = userEvent.setup();
    const installed = plugin({ id: "p2", name: "http-json", kind: "check", title: "Web address", places: [] });
    stubFetch(routes([], { "POST /api/plugins/inspect": { status: 200, body: inspection() } }));
    renderPage(single(globalAdmin));
    const file = new File(["{}"], "http-json-1.0.0.mfplugin");
    await user.upload(await screen.findByLabelText(/Plugin file/), file);
    const asks = await screen.findByRole("list", { name: "What it asks for" });
    expect(within(asks).getByText("Connects to the address in its Address setting")).toBeInTheDocument();
    expect(screen.getByText(/Signed by Acme IT, key fedcba9876543210/)).toBeInTheDocument();
    // Installed: the list now has it.
    stubFetch(
      routes([installed], {
        [`POST /api/plugins?sha256=${"ab".repeat(32)}`]: { status: 201, body: installed },
      }),
    );
    await user.click(screen.getByRole("button", { name: "Install" }));
    expect(await screen.findByRole("heading", { name: "Where Web address may be used" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Every server" })).not.toBeChecked();
  });

  it("explains a file it cannot install", async () => {
    const user = userEvent.setup();
    stubFetch(
      routes([], {
        "POST /api/plugins/inspect": {
          status: 400,
          body: { error: "This plugin is signed by a key you do not trust yet (fingerprint 0011223344556677)." },
        },
      }),
    );
    renderPage();
    await user.upload(await screen.findByLabelText(/Plugin file/), new File(["{}"], "x.mfplugin"));
    expect(await screen.findByRole("alert")).toHaveTextContent("a key you do not trust yet");
    expect(screen.queryByRole("button", { name: "Install" })).not.toBeInTheDocument();
  });

  it("names only what an update adds", async () => {
    const user = userEvent.setup();
    stubFetch(
      routes([], {
        "POST /api/plugins/inspect": {
          status: 200,
          body: inspection({
            version: "1.1.0",
            update: { from_version: "1.0.0", adds: ["Connects to api.example.com"], drops: [] },
          }),
        },
      }),
    );
    renderPage();
    await user.upload(await screen.findByLabelText(/Plugin file/), new File(["{}"], "x.mfplugin"));
    const asks = await screen.findByRole("list", { name: "What it asks for" });
    expect(within(asks).getAllByRole("listitem").map((li) => li.textContent)).toEqual(["Connects to api.example.com"]);
    expect(screen.getByRole("button", { name: "Update to 1.1.0" })).toBeInTheDocument();
  });

  it("trusts a publisher's key", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch(
      routes([], {
        "POST /api/plugins/publishers": {
          status: 201,
          body: { id: "k1", name: "Acme IT", public_key: "AAAA", fingerprint: "00", plugins: [], created_at: "" },
        },
      }),
    );
    renderPage();
    await user.type(await screen.findByLabelText("Name"), "Acme IT");
    await user.type(screen.getByLabelText("Public key"), "MCowBQYDK2VwAyEA");
    await user.click(screen.getByRole("button", { name: "Trust this publisher" }));
    const call = fetchMock.mock.calls.find(([url, init]) => url === "/api/plugins/publishers" && init?.method === "POST");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ name: "Acme IT", public_key: "MCowBQYDK2VwAyEA" });
  });
});

describe("placeChoices", () => {
  it("shows customers only to someone who sees several, sites once there are several", () => {
    const acme = [customer("c1", "acme")];
    const one = placeChoices(acme, [site("s1", "main", "c1", "acme")], false);
    expect(one.map((c) => c.label)).toEqual(["Every server"]);
    const sites = placeChoices(acme, [site("s1", "paris", "c1", "acme"), site("s2", "lyon", "c1", "acme")], false);
    expect(sites.map((c) => c.label)).toEqual(["Every server", "paris", "lyon"]);
    const msp = placeChoices(
      [customer("c1", "acme"), customer("c2", "globex")],
      [site("s1", "main", "c1", "acme"), site("s2", "hq", "c2", "globex")],
      true,
    );
    expect(msp.map((c) => c.label)).toEqual(["Every customer", "All of acme", "All of globex", "acme / main", "globex / hq"]);
  });
});

describe("placesSummary", () => {
  it("says where in words", () => {
    expect(placesSummary(plugin({ places: [] }), false)).toBe("Nowhere yet");
    const places: InstalledPlugin["places"] = [
      { customer_id: "c1", customer_name: "acme", site_id: null, site_name: null },
      { customer_id: "c2", customer_name: "globex", site_id: "s2", site_name: "hq" },
    ];
    expect(placesSummary(plugin({ places }), true)).toBe("All of acme, globex / hq");
    expect(placesSummary(plugin({ places: places.slice(1) }), false)).toBe("hq");
    // A customer place said without the customer, for one customer.
    expect(placesSummary(plugin({ places: places.slice(0, 1) }), false)).toBe("All sites");
  });
});
