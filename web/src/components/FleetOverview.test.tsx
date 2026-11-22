import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { en } from "@/i18n/messages";
import type { Server, ServerState } from "@/lib/api";
import { isServerState, SERVER_STATES, stateWord } from "@/lib/serverState";
import { As, customerAdmin, me, oneSite, single as oneCustomer, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { FleetOverview, REFRESH_MS } from "./FleetOverview";
import { GROUP_PAGE } from "./FleetTree";

vi.mock("next/navigation", () => import("@/test/navigation"));

const NOW = new Date("2026-10-22T14:32:05");

/**
 * What `t` shows for a key the catalog lacks: `area.name`, the area one of
 * the catalog's. Cells run together in `textContent` ("0.1.0common.state"),
 * so no word boundary.
 */
const CATALOG_KEY = new RegExp(`(?<![A-Za-z])(?:${Object.keys(en).join("|")})\\.[A-Za-z_][\\w.]*`);

function server(overrides: Partial<Server>): Server {
  return {
    id: "s1",
    peer_id: "agent-1",
    hostname: "web-1",
    site_id: "site-1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    relay_id: null,
    os_family: "debian",
    os_version: "12",
    agent_version: "0.1.0",
    created_at: "2026-10-18T10:00:00Z",
    revoked_at: null,
    status: "online",
    connected_at: "2026-10-18T11:00:00Z",
    last_heartbeat: new Date(NOW.getTime() - 12_000).toISOString(),
    via_relay: null,
    open_incidents: 0,
    cert_not_after: new Date(NOW.getTime() + 60 * 86_400_000).toISOString(),
    ...overrides,
  };
}

const web1 = server({ open_incidents: 1, state: "critical", via_relay: "relay-paris-1" });
const db1 = server({
  id: "s2",
  hostname: "db-1",
  os_family: "rhel",
  os_version: "9",
  agent_version: null,
  status: "offline",
  state: "offline",
  connected_at: null,
  last_heartbeat: new Date(2026, 9, 22, 13, 51).toISOString(),
});
const mail = server({ id: "s4", hostname: "mail-1", cert_not_after: new Date(NOW.getTime() + 1.4 * 86_400_000).toISOString() });
const disk = incident({ server_id: "s1", opened_at: new Date(NOW.getTime() - (4 * 3600 + 20 * 60) * 1000).toISOString(), last_value: 0.968 });

const SERVERS = "GET /api/servers";
const OPEN = "GET /api/incidents?state=open&limit=500";
const reply = (servers: Server[], incidents = [disk]) => ({
  [SERVERS]: { status: 200, body: { servers, next_cursor: null } },
  [OPEN]: { status: 200, body: { incidents, next_cursor: null } },
});

const table = () => screen.findByRole("table", { name: "Servers", busy: false });
/** Server rows, without group lines. */
const rows = async () => [...(await table()).querySelectorAll<HTMLElement>("tbody tr[data-row]")];
/** Group lines: "paris  3 servers  1 critical …". */
const groups = async () => [...(await table()).querySelectorAll<HTMLElement>("tbody th[scope=rowgroup]")];

describe("FleetOverview", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl("/");
  });
  afterEach(() => vi.useRealTimers());

  it("lists servers problems first, with one status and a problem per row", async () => {
    stubFetch(reply([mail, db1, web1]));
    render(<FleetOverview />);
    const [first, second, third] = await rows();
    expect(first).toHaveTextContent(/web-1Debian 12 · agent 0\.1\.0 · via relay-paris-1\s*Critical\s*Disk used \/var 96\.8% · 4 h 20 min\s*12 s ago/);
    expect(second).toHaveTextContent(/db-1RHEL 9 · agent unknown\s*Offline\s*Last contact 13:51/);
    expect(third).toHaveTextContent(/mail-1.*OK\s*Warning\s+Certificate: Renewal late, expires in 1 day/);
    expect(within(first!).getByRole("link", { name: "web-1" })).toHaveAttribute("href", expect.stringMatching(/^\/servers\/view\/?\?id=s1$/));
    // The status strip of the shell replaced the counters.
    expect(screen.queryByRole("group", { name: "Summary, click to filter" })).not.toBeInTheDocument();
  });

  it("speaks French when that is the language", async () => {
    setActiveLocale("fr");
    const until = new Date(2026, 9, 22, 18, 0).toISOString();
    const quiet = server({ id: "s6", hostname: "app-1", state: "silenced", silence: { by: "alice", until, reason: "migration" } });
    stubFetch(reply([quiet, db1, web1]));
    render(<FleetOverview />);
    const [web, db] = [...(await screen.findByRole("table", { name: "Serveurs", busy: false })).querySelectorAll("tbody tr[data-row]")];
    expect(web).toHaveTextContent(/web-1Debian 12 · agent 0\.1\.0 · via relay-paris-1\s*Critique/);
    expect(db).toHaveTextContent(/agent inconnu\s*Hors ligne\s*Dernier contact 13:51/);
    expect(screen.getByText(/^par alice jusqu’à aujourd’hui 18:00 · migration$/)).toBeInTheDocument();
    expect(screen.getByRole("columnheader", { name: "Problème" })).toBeInTheDocument();
  });

  it("shows silenced and acknowledged servers after the problems, and filters by status", async () => {
    const until = new Date(2026, 9, 22, 18, 0).toISOString();
    const quiet = server({
      id: "s6",
      hostname: "app-1",
      open_incidents: 1,
      state: "silenced",
      silenced_until: until,
      silence: { by: "alice", until, reason: "migration" },
    });
    const taken = server({
      id: "s7",
      hostname: "app-2",
      open_incidents: 1,
      state: "acknowledged",
      acknowledgement: { by: "bob", at: new Date(2026, 9, 22, 14, 2).toISOString(), note: "replacing the disk" },
    });
    stubFetch(reply([quiet, taken, web1, db1, mail], []));
    render(<FleetOverview />);
    // After the checkbox and the host.
    const order = (await rows()).map((r) => within(r).getAllByRole("cell")[2]!.textContent);
    expect(order).toEqual(["Critical", "Offline", "OK", "Acknowledged", "Silenced"]);
    // Who took them out of the problems, until when, and why (ADR-0130).
    // The status label already says Silenced or Acknowledged.
    expect(screen.getByText("by alice until 18:00 today · migration")).toBeInTheDocument();
    expect(screen.getByText("by bob 14:02 · replacing the disk")).toBeInTheDocument();
  });

  it("says a server is in maintenance during its window", async () => {
    const until = new Date(2026, 9, 23, 4, 0).toISOString();
    const quiet = server({ id: "s6", hostname: "app-1", state: "silenced", silence: { by: "alice", until, reason: "Patch night", window: true } });
    stubFetch(reply([quiet, web1], []));
    render(<FleetOverview />);
    await rows();
    expect(screen.getByText(/^in maintenance until .*04:00 · Patch night$/)).toBeInTheDocument();
  });

  it("filters by state from the status bar's link, with no second state filter", async () => {
    setUrl("/?state=silenced");
    const until = new Date(2026, 9, 22, 18, 0).toISOString();
    const quiet = server({ id: "s6", hostname: "app-1", state: "silenced", silence: { by: "alice", until, reason: "migration" } });
    stubFetch(reply([quiet, web1, db1], []));
    render(<FleetOverview />);
    const shown = await rows();
    expect(screen.queryByRole("combobox", { name: "Status" })).toBeNull();
    expect(shown).toHaveLength(1);
    expect(shown[0]).toHaveTextContent("app-1");
  });

  it("filters from the URL and keeps the scope", async () => {
    setUrl("/?customer=0b8f3c4e-1a2b-4c3d-8e9f-000000000001&state=offline");
    stubFetch(reply([web1, db1, server({ id: "s9", hostname: "other", customer_id: "0b8f3c4e-1a2b-4c3d-8e9f-000000000002" }), { ...db1, id: "s5", hostname: "db-2", customer_id: "0b8f3c4e-1a2b-4c3d-8e9f-000000000001" }]));
    render(<FleetOverview />);
    const shown = await rows();
    expect(shown).toHaveLength(1);
    expect(shown[0]).toHaveTextContent("db-2");

    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(window.location.search).toBe("?customer=0b8f3c4e-1a2b-4c3d-8e9f-000000000001");
    expect(await rows()).toHaveLength(1);
  });

  it("searches hosts and keeps servers with incidents", async () => {
    stubFetch(reply([web1, db1, mail]));
    render(<FleetOverview />);
    await rows();
    await userEvent.click(screen.getByRole("button", { name: "Has incidents" }));
    expect(await rows()).toHaveLength(1);
    expect(window.location.search).toBe("?incidents=1");
    await userEvent.click(screen.getByRole("button", { name: "Has incidents" }));
    await userEvent.type(screen.getByRole("searchbox", { name: "Search" }), "db");
    const shown = await rows();
    expect(shown).toHaveLength(1);
    expect(shown[0]).toHaveTextContent("db-1");
  });

  it("groups servers by site, folds healthy sites and names a lone problem", async () => {
    const at = (site: string, n: number) => ({ site_id: site, site_name: site, id: `${site}-${n}`, hostname: `${site}-web-${n}` });
    const fine = Array.from({ length: 3 }, (_, n) => server(at("lyon", n)));
    const busy = [
      { ...web1, site_id: "berlin", site_name: "berlin" },
      server({ ...at("berlin", 2), state: "warning", open_incidents: 1 }),
      server({ ...at("berlin", 3), state: "warning", open_incidents: 1 }),
      server(at("berlin", 4)),
    ];
    stubFetch(reply([...fine, ...busy]));
    render(
      <As user={oneCustomer(viewer)}>
        <FleetOverview />
      </As>,
    );
    const [first, second] = await groups();
    // Problems first; the healthy site is one line.
    expect(first).toHaveTextContent(/^berlin4 servers\s*Critical: web-1 · Disk used \/var 96\.8%\s*2 warning\s*1 OK$/);
    expect(second).toHaveTextContent(/^lyon3 servers\s*3 OK$/);
    expect(within(first!).getByRole("link", { name: /web-1 · Disk/ })).toHaveAttribute("href", expect.stringMatching(/^\/servers\/view\/?\?id=s1$/));
    expect(within(first!).getByRole("link", { name: "2 warning" })).toHaveAttribute("href", "/?state=warning&site=berlin");
    // Its OK server waits behind a button.
    expect(await rows()).toHaveLength(3);
    await userEvent.click(screen.getByRole("button", { name: "Show 1 OK server of berlin" }));
    expect(await rows()).toHaveLength(4);

    const lyon = within(second!).getByRole("button", { name: /lyon/ });
    expect(lyon).toHaveAttribute("aria-expanded", "false");
    await userEvent.click(lyon);
    expect(lyon).toHaveAttribute("aria-expanded", "true");
    expect(await rows()).toHaveLength(7);
    await userEvent.click(within(first!).getByRole("button", { name: /berlin/ }));
    expect(await rows()).toHaveLength(3);

    // A filter opens every group it leaves.
    await userEvent.type(screen.getByRole("searchbox", { name: "Search" }), "lyon-web-1");
    expect((await rows()).map((r) => r.textContent)).toEqual([expect.stringContaining("lyon-web-1")]);

    // A sort orders the servers within each site.
    await userEvent.clear(screen.getByRole("searchbox", { name: "Search" }));
    await userEvent.click(screen.getByRole("button", { name: "Server" }));
    expect(window.location.search).toBe("?sort=host");
    await userEvent.click(screen.getByRole("button", { name: "Server" }));
    expect(window.location.search).toBe("?sort=-host");
    // A third click goes back to problems first.
    await userEvent.click(screen.getByRole("button", { name: "Server" }));
    expect(window.location.search).toBe("");
  });

  it("shows customers above sites only to someone who sees two", async () => {
    const other = server({ id: "s8", hostname: "shop-1", customer_id: "c2", customer_name: "globex", site_id: "site-9", site_name: "rome" });
    stubFetch(reply([web1, other]));
    render(<FleetOverview />);
    const lines = (await groups()).map((g) => g.textContent);
    expect(lines).toEqual([
      expect.stringMatching(/^acme1 server/),
      expect.stringMatching(/^paris1 server/),
      expect.stringMatching(/^globex1 server\s*1 OK$/),
    ]);
    expect(screen.getByRole("link", { name: "1 OK" })).toHaveAttribute("href", "/?state=ok&customer=c2");
  });

  it("lists one site without a group line, its OK servers behind a button", async () => {
    stubFetch(reply([web1, mail, server({ id: "s3", hostname: "ok-1" })]));
    render(
      <As user={oneCustomer(viewer)}>
        <FleetOverview />
      </As>,
    );
    expect(await rows()).toHaveLength(2);
    expect(await groups()).toHaveLength(0);
    expect(screen.getByRole("columnheader", { name: "Site" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Show 1 OK server" }));
    expect(await rows()).toHaveLength(3);
  });

  it("names no site while there is only one", async () => {
    stubFetch(reply([web1, mail]));
    render(
      <As user={oneSite(viewer)}>
        <FleetOverview />
      </As>,
    );
    await rows();
    expect(screen.queryByRole("columnheader", { name: "Site" })).toBeNull();
    expect(screen.queryByRole("combobox", { name: "Site" })).toBeNull();
    expect(screen.queryByText("paris")).toBeNull();
  });

  it("shows a large site 50 servers at a time", async () => {
    const many = Array.from({ length: GROUP_PAGE + 5 }, (_, i) => server({ id: `m${i}`, hostname: `node-${i}`, state: "warning", open_incidents: 1 }));
    stubFetch(reply([...many, server({ id: "o1", site_id: "other", site_name: "other" })], []));
    render(
      <As user={oneCustomer(viewer)}>
        <FleetOverview />
      </As>,
    );
    expect(await rows()).toHaveLength(GROUP_PAGE);
    await userEvent.click(screen.getByRole("button", { name: "Show 5 more of paris" }));
    expect(await rows()).toHaveLength(GROUP_PAGE + 5);
  });

  // Every state the central server sends, and one a newer server might add,
  // in both languages: a word in each row, never a raw catalog key such as
  // `common.state.waiting`.
  it.each(["en", "fr"] as const)("shows a word for every state, never a catalog key (%s)", async (locale) => {
    setActiveLocale(locale);
    const states = [...SERVER_STATES, "draining" as ServerState];
    const servers = states.map((state, i) =>
      server({ id: `s${i}`, hostname: `host-${state}`, state, open_incidents: state === "warning" || state === "critical" ? 1 : 0 }),
    );
    stubFetch(reply(servers, []));
    const { container } = render(<FleetOverview />);
    await screen.findByRole("table", { busy: false });
    expect(container.textContent).not.toMatch(CATALOG_KEY);
    // A search opens every group, OK servers included.
    await userEvent.type(screen.getByRole("searchbox"), "host-");
    for (const state of states) {
      const row = screen.getByRole("link", { name: `host-${state}` }).closest("tr")!;
      expect(row, state).toHaveTextContent(stateWord(isServerState(state) ? state : "ok"));
    }
    expect(container.textContent).not.toMatch(CATALOG_KEY);
  });

  it("checks the servers shown and acknowledges their alerts, each server on its own", async () => {
    const berlin = server({ id: "s5", hostname: "web-9", site_id: "site-2", site_name: "berlin" });
    const fetchMock = stubFetch({
      ...reply([web1, db1, server({ id: "s3", hostname: "web-2" }), berlin]),
      "POST /api/incidents/acknowledge": { status: 200, body: { acknowledged: [disk.id] } },
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <As user={customerAdmin}>
        <FleetOverview />
      </As>,
    );
    await table();
    // paris shows its two problems, web-2 folds away as OK and berlin is folded: neither is checked.
    await user.click(screen.getByRole("checkbox", { name: "Select every row shown" }));
    const bar = screen.getByRole("region", { name: "Bulk actions" });
    expect(within(bar).getByText("2 selected")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Select web-1" })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: "Select db-1" })).toBeChecked();
    expect(screen.queryByRole("checkbox", { name: "Select web-9" })).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: "Select web-2" })).not.toBeInTheDocument();

    await user.click(within(bar).getByRole("button", { name: "Acknowledge the alerts of the 2 selected servers" }));
    await user.type(within(bar).getByRole("textbox", { name: "What are you doing about it?" }), "on it{Enter}");
    expect(await within(bar).findByRole("status")).toHaveTextContent(/^1 acknowledged, 1 with no alert to acknowledge/);
    const bodies = fetchMock.mock.calls
      .filter(([url]) => url === "/api/incidents/acknowledge")
      .map(([, init]) => JSON.parse(String(init?.body)));
    expect(bodies).toEqual([{ incident_ids: [disk.id], note: "on it" }]);
    expect(within(bar).queryByText(/selected/)).not.toBeInTheDocument();
  });

  it("silences the checked servers the user may change", async () => {
    const other = server({ id: "s5", hostname: "db-2", site_id: "site-2", site_name: "berlin", state: "critical", open_incidents: 1 });
    const fetchMock = stubFetch({ ...reply([web1, other]), "POST /api/silences": { status: 201, body: {} } });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <As user={me({ role: "operator", customer_id: null, site_id: "site-1" })}>
        <FleetOverview />
      </As>,
    );
    await user.click(await screen.findByRole("checkbox", { name: "Select web-1" }));
    await user.click(screen.getByRole("checkbox", { name: "Select db-2" }));
    await user.click(screen.getByRole("button", { name: "Silence the 2 selected servers" }));
    await user.click(screen.getByRole("menuitem", { name: "Until tomorrow, 8:00" }));
    expect(await screen.findByText(/^1 silenced, 1 refused: insufficient rights \(db-2\)/)).toBeInTheDocument();
    const bodies = fetchMock.mock.calls.filter(([url]) => url === "/api/silences").map(([, init]) => JSON.parse(String(init?.body)));
    expect(bodies).toEqual([
      { scope: "server", target_id: "s1", ends_at: new Date(2026, 9, 23, 8).toISOString(), reason: "Planned maintenance" },
    ]);
    // The refused one stays checked.
    expect(screen.getByRole("checkbox", { name: "Select db-2" })).toBeChecked();
  });

  describe("administration of the checked servers", () => {
    const admin = me({ role: "administrator", customer_id: "c1", site_id: null });
    const other = server({ id: "s5", hostname: "db-2", customer_id: "c2", customer_name: "globex", site_id: "site-2", site_name: "berlin", state: "critical", open_incidents: 1 });
    const device = server({ id: "s6", hostname: "switch-1", kind: "device", agent_version: null, state: "critical", open_incidents: 1 });
    const current = server({ id: "s7", hostname: "web-7", agent_version: "0.2.0", state: "critical", open_incidents: 1, offline_alerts: false });
    const releases = { "GET /api/agent-releases": { status: 200, body: { versions: ["0.2.0", "0.1.0"], latest: "0.2.0" } } };

    async function checkAll(servers: Server[], routes: Record<string, { status: number; body?: unknown }>) {
      const fetchMock = stubFetch({ ...reply(servers, []), ...releases, ...routes });
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      render(
        <As user={admin}>
          <FleetOverview />
        </As>,
      );
      await table();
      await user.click(screen.getByRole("checkbox", { name: "Select every row shown" }));
      const bar = screen.getByRole("region", { name: "Bulk actions" });
      await user.click(within(bar).getByRole("button", { name: `More actions for the ${servers.length} selected servers` }));
      return { fetchMock, user, bar };
    }
    const posted = (fetchMock: ReturnType<typeof stubFetch>, method: string) =>
      fetchMock.mock.calls.filter(([, init]) => init?.method === method).map(([url]) => url);

    it("removes them after a confirmation that gives the count", async () => {
      const { fetchMock, user, bar } = await checkAll([web1, other], {
        "POST /api/servers/s1/revoke": { status: 200, body: {} },
      });
      await user.click(screen.getByRole("menuitem", { name: "Remove…" }));
      expect(within(bar).getByText("Remove the 2 selected servers?")).toBeInTheDocument();
      expect(posted(fetchMock, "POST")).toEqual([]);
      await user.click(within(bar).getByRole("button", { name: "Remove 2 servers" }));
      expect(await within(bar).findByRole("status")).toHaveTextContent(/^1 removed, 1 refused: insufficient rights \(db-2\)/);
      expect(posted(fetchMock, "POST")).toEqual(["/api/servers/s1/revoke"]);
    });

    it("updates the agents that are behind", async () => {
      const { fetchMock, user, bar } = await checkAll([web1, device, current], {
        "POST /api/servers/s1/commands": { status: 201, body: {} },
      });
      await user.click(screen.getByRole("menuitem", { name: "Update agents to 0.2.0…" }));
      await user.click(within(bar).getByRole("button", { name: "Update 3 servers" }));
      expect(await within(bar).findByRole("status")).toHaveTextContent(/^1 queued for update, 2 already up to date or without an agent/);
      expect(posted(fetchMock, "POST")).toEqual(["/api/servers/s1/commands"]);
    });

    it("turns offline alerts off where they are on", async () => {
      const { fetchMock, user, bar } = await checkAll([web1, current], {
        "PUT /api/servers/s1/offline-alerts": { status: 200, body: { enabled: false } },
      });
      await user.click(screen.getByRole("menuitem", { name: "Turn off offline alerts" }));
      expect(await within(bar).findByRole("status")).toHaveTextContent(/^1 changed, 1 already that way/);
      expect(posted(fetchMock, "PUT")).toEqual(["/api/servers/s1/offline-alerts"]);
    });

    it("keeps a maintenance window to one site", async () => {
      const { user, bar } = await checkAll([web1, other], {});
      await user.click(screen.getByRole("menuitem", { name: "Maintenance window…" }));
      expect(await within(bar).findByRole("status")).toHaveTextContent(/^A maintenance window covers servers of one site\./);
    });
  });

  it("falls back to the servers' counts when incidents cannot be read", async () => {
    stubFetch({ ...reply([web1]), [OPEN]: { status: 500, body: { error: "internal error" } } });
    render(<FleetOverview />);
    const [row] = await rows();
    expect(within(row!).getByRole("link", { name: "1 open incident" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/incidents\/?\?server=s1$/),
    );
  });

  it("offers enrollment to administrators, with an empty state", async () => {
    stubFetch(reply([], []));
    const { unmount } = render(
      <As user={customerAdmin}>
        <FleetOverview />
      </As>,
    );
    expect(await screen.findByText("No servers yet")).toBeInTheDocument();
    expect(screen.getAllByRole("link", { name: "Add servers" })[0]).toHaveAttribute("href", expect.stringMatching(/^\/settings\/add-servers\/?$/));
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
    unmount();

    render(
      <As user={viewer}>
        <FleetOverview />
      </As>,
    );
    expect(await screen.findByText("No servers yet")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Add servers" })).not.toBeInTheDocument();
  });

  it("explains a 403", async () => {
    stubFetch({ [SERVERS]: { status: 403, body: { error: "forbidden" } }, [OPEN]: { status: 403, body: { error: "forbidden" } } });
    render(<FleetOverview />);
    expect(await screen.findByRole("alert")).toHaveTextContent("You do not have access");
  });

  it("refreshes periodically and keeps the last data when a refresh fails", async () => {
    const fetchMock = stubFetch({
      [SERVERS]: [
        { status: 200, body: { servers: [web1], next_cursor: null } },
        { status: 200, body: { servers: [web1, db1], next_cursor: null } },
        { status: 502, body: { error: "bad gateway" } },
      ],
      [OPEN]: { status: 200, body: { incidents: [], next_cursor: null } },
    });
    render(<FleetOverview />);
    expect(await screen.findByRole("link", { name: "web-1" })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "db-1" })).not.toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByRole("link", { name: "db-1" })).toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    // The bulk bar's live region is a status too, empty until an action runs.
    await vi.waitFor(() =>
      expect(screen.getAllByRole("status").map((s) => s.textContent)).toContainEqual(expect.stringMatching(/Refresh failed.*last data received/)),
    );
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(fetchMock.mock.calls.filter(([url]) => url === "/api/servers")).toHaveLength(3);
  });
});
