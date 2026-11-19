import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { Dependency, DependencyEnd, DependencyPage, IncidentDetail, Server } from "@/lib/api";
import { endWords } from "@/lib/dependencies";
import { stubFetch } from "@/test/fetch";

import { DependenciesTab, IncidentDependencies, RelayDependencies, UnreachableBanner } from "./Dependencies";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ok = (body: unknown) => ({ status: 200, body });

const end = (overrides: Partial<DependencyEnd>): DependencyEnd => ({
  kind: "server",
  server_id: "s1",
  relay_id: null,
  name: "web-1",
  site_id: "site-1",
  check: null,
  ...overrides,
});
const web = end({});
const sw = end({ kind: "device", server_id: "d1", name: "sw-core" });
const fw = end({ kind: "device", server_id: "d2", name: "fw-1" });
const gw = end({ kind: "relay", server_id: null, relay_id: "r1", name: "gw-1" });

const dependency = (overrides: Partial<Dependency>): Dependency => ({
  id: "x1",
  automatic: false,
  child: web,
  parent: sw,
  created_at: "2026-11-18T09:00:00Z",
  created_by: "alice",
  can_change: true,
  ...overrides,
});

const page = (overrides: Partial<DependencyPage> = {}): DependencyPage => ({
  parents: [],
  children: [],
  behind: [],
  suggestions: [],
  unreachable: null,
  unreachable_checks: [],
  can_change: true,
  ...overrides,
});

const server = { id: "s1", hostname: "web-1", customer_id: "c1", site_id: "site-1" } as Server;
const fleet = ok({
  servers: [
    { id: "s1", hostname: "web-1", customer_id: "c1", site_id: "site-1", revoked_at: null },
    { id: "d1", hostname: "sw-core", customer_id: "c1", site_id: "site-1", revoked_at: null, kind: "device" },
    { id: "d2", hostname: "fw-1", customer_id: "c1", site_id: "site-1", revoked_at: null, kind: "device" },
    { id: "o1", hostname: "theirs", customer_id: "c2", site_id: "site-9", revoked_at: null },
  ],
  next_cursor: null,
});
const relays = ok({ relays: [{ id: "r1", name: "gw-1", customer_id: "c1", revoked_at: null }], next_cursor: null });

const bodyOf = (fetchMock: ReturnType<typeof stubFetch>) =>
  JSON.parse(String(fetchMock.mock.calls.find((c) => c[1]?.method === "POST")?.[1]?.body)) as unknown;

afterEach(() => {
  vi.unstubAllGlobals();
  setActiveLocale("en");
});

describe("dependencies", () => {
  it("names ends in words", () => {
    expect(endWords(sw)).toBe("sw-core");
    expect(endWords(gw)).toBe("relay gw-1");
    expect(endWords({ ...sw, check: { name: "ping", instance: "" } })).toBe("ping on sw-core");
    expect(endWords({ ...sw, check: { name: "port", instance: "443" } })).toBe("port (443) on sw-core");
  });

  it("lists what a server depends on, automatic ones included, and adds a suggestion in one click", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/servers/s1/dependencies": ok(
        page({
          parents: [
            dependency({ id: null, automatic: true, parent: gw, created_at: null, created_by: null, can_change: false }),
            dependency({}),
          ],
          suggestions: [{ parent: fw, reason: "network_device" }],
        }),
      ),
      "POST /api/dependencies": { status: 201, body: { dependencies: [] } },
    });
    render(<DependenciesTab server={server} checks={["cpu", "https"]} />);
    const table = await screen.findByRole("table", { name: "Depends on" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent)).toEqual([
      expect.stringContaining("All of web-1relay gw-1Automatic"),
      expect.stringContaining("All of web-1sw-coreby alice"),
    ]);
    // Only the one set by someone can be removed.
    expect(within(rows[0]!).queryByRole("button")).not.toBeInTheDocument();
    const suggested = screen.getByRole("group", { name: "Suggested" });
    expect(suggested).toHaveTextContent("fw-1 A switch, router or firewall of the same site");
    await user.click(within(suggested).getByRole("button", { name: "Add" }));
    expect(bodyOf(fetchMock)).toEqual({ parent: { server_id: "d2" }, children: [{ server_id: "s1" }] });
  });

  it("puts several servers behind a device at once, and says why it was refused", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/servers/d1/dependencies": ok(page()),
      "GET /api/servers": fleet,
      "GET /api/relays": relays,
      "POST /api/dependencies": {
        status: 409,
        body: { error: "sw-core cannot depend on web-1: web-1 already depends on sw-core (web-1, then sw-core).", code: "conflict" },
      },
    });
    const device = { id: "d1", hostname: "sw-core", customer_id: "c1", site_id: "site-1" } as Server;
    render(<DependenciesTab server={device} checks={["ping"]} />);
    await user.click(await screen.findByRole("button", { name: "Add what is behind it" }));
    const form = screen.getByRole("form", { name: "Add what is behind it" });
    const choices = within(form).getByRole("group", { name: "Behind it" });
    // Itself and other customers' are not offered.
    await within(choices).findByText("web-1");
    expect(within(choices).queryByText("sw-core")).not.toBeInTheDocument();
    expect(within(choices).queryByText("theirs")).not.toBeInTheDocument();
    await user.click(within(choices).getByRole("checkbox", { name: "web-1" }));
    await user.click(within(choices).getByRole("checkbox", { name: "fw-1" }));
    await user.selectOptions(within(form).getByRole("combobox", { name: "Behind" }), "ping");
    await user.click(within(form).getByRole("button", { name: "Add" }));
    expect(bodyOf(fetchMock)).toEqual({
      parent: { server_id: "d1", check: { name: "ping", instance: "" } },
      children: [{ server_id: "s1" }, { server_id: "d2" }],
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "sw-core cannot depend on web-1: web-1 already depends on sw-core (web-1, then sw-core).",
    );
  });

  it("shows what is behind a relay without a Depends on section", async () => {
    stubFetch({
      "GET /api/relays/r1/dependencies": ok(
        page({
          children: [dependency({ id: null, automatic: true, parent: gw, can_change: false, created_at: null, created_by: null })],
          behind: [web],
        }),
      ),
    });
    render(<RelayDependencies relay={{ id: "r1", name: "gw-1", customer_id: "c1" }} />);
    const table = await screen.findByRole("table", { name: "Behind it" });
    expect(within(table).getAllByRole("row")[1]).toHaveTextContent("web-1relay gw-1Automatic");
    expect(screen.queryByRole("region", { name: "Depends on" })).not.toBeInTheDocument();
  });

  it("says why a server is unreachable, in English and French", () => {
    const note = { because: sw, incident_id: "i1", since: "2026-11-18T10:00:00Z" };
    const { unmount } = render(<UnreachableBanner name="web-1" note={note} />);
    expect(screen.getByText("Unreachable: sw-core is down.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "See its incident" })).toHaveAttribute("href", expect.stringContaining("i1"));
    unmount();
    setActiveLocale("fr");
    render(<UnreachableBanner name="web-1" note={note} />);
    expect(screen.getByText(/^Injoignable\s:\ssw-core est en panne\.$/)).toBeInTheDocument();
  });

  it("lists on an incident what it hides, and on a hidden one what speaks for it", () => {
    const incident = { id: "i1", hides: [web, { ...fw, check: { name: "https", instance: "" } }], hidden: ["i9"], hidden_by: null } as unknown as IncidentDetail;
    const { unmount } = render(<IncidentDependencies incident={incident} />);
    const section = screen.getByRole("region", { name: "While this is down, 2 are unreachable behind it" });
    expect(within(section).getAllByRole("link").map((l) => l.textContent)).toEqual(["web-1", "https on fw-1"]);
    expect(screen.getByRole("link", { name: "Incident 1" })).toHaveAttribute("href", expect.stringContaining("i9"));
    unmount();
    render(<IncidentDependencies incident={{ ...incident, hides: [], hidden: [], hidden_by: "i1" }} />);
    expect(screen.getByText(/Nobody was told of this incident/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "See the incident that speaks for it" })).toHaveAttribute("href", expect.stringContaining("i1"));
  });
});
