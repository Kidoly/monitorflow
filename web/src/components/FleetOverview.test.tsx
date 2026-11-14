import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Server } from "@/lib/api";
import { As, customerAdmin, single as oneCustomer, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { FleetOverview, REFRESH_MS } from "./FleetOverview";
import { GROUP_PAGE } from "./FleetTree";

vi.mock("next/navigation", () => import("@/test/navigation"));

const NOW = new Date("2026-10-22T14:32:05");

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
    expect(screen.getByText(/3 servers, 2 online · Updated 14:32:05, refreshes every 15 seconds/)).toBeInTheDocument();
    // The status strip of the shell replaced the counters.
    expect(screen.queryByRole("group", { name: "Summary, click to filter" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Problems first" })).toBeInTheDocument();
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
    const order = (await rows()).map((r) => within(r).getAllByRole("cell")[1]!.textContent);
    expect(order).toEqual(["Critical", "Offline", "OK", "Acknowledged", "Silenced"]);
    // Who took them out of the problems, until when, and why (ADR-0130).
    expect(screen.getByText("Silenced by alice until 18:00 today: migration")).toBeInTheDocument();
    expect(screen.getByText("Acknowledged by bob 14:02: replacing the disk")).toBeInTheDocument();

    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Status" }), "silenced");
    expect(window.location.search).toBe("?state=silenced");
    const shown = await rows();
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
    expect(screen.getByText(/1 server, 0 online/)).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Status" })).toHaveValue("offline");

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
    await userEvent.click(screen.getByRole("button", { name: "Problems first" }));
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

  it("falls back to the servers' counts when incidents cannot be read", async () => {
    stubFetch({ ...reply([web1]), [OPEN]: { status: 500, body: { error: "internal error" } } });
    render(<FleetOverview />);
    const [row] = await rows();
    expect(within(row!).getByRole("link", { name: "1 open incident" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/incidents\/?\?state=open&server=s1$/),
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
    expect(await screen.findByText(/1 server, 1 online/)).toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByText(/2 servers, 1 online/)).toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByRole("status", { name: "" })).toHaveTextContent(/Refresh failed.*last data received/);
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(fetchMock.mock.calls.filter(([url]) => url === "/api/servers")).toHaveLength(3);
  });
});
