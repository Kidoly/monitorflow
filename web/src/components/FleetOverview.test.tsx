import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Server } from "@/lib/api";
import { As, customerAdmin, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { FleetOverview, HEALTHY_SHOWN, REFRESH_MS } from "./FleetOverview";

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

const web1 = server({ open_incidents: 1, via_relay: "relay-paris-1" });
const db1 = server({
  id: "s2",
  hostname: "db-1",
  os_family: "rhel",
  os_version: "9",
  agent_version: null,
  status: "offline",
  connected_at: null,
  last_heartbeat: new Date(2026, 9, 22, 13, 51).toISOString(),
});
const old = server({ id: "s3", hostname: "web-10", status: "offline", revoked_at: "2026-10-14T10:00:00Z" });
const mail = server({ id: "s4", hostname: "mail-1", cert_not_after: new Date(NOW.getTime() + 5.5 * 86_400_000).toISOString() });
const disk = incident({ server_id: "s1", opened_at: new Date(NOW.getTime() - (4 * 3600 + 20 * 60) * 1000).toISOString(), last_value: 0.968 });

const SERVERS = "GET /api/servers";
const OPEN = "GET /api/incidents?state=open&limit=500";
const reply = (servers: Server[], incidents = [disk]) => ({
  [SERVERS]: { status: 200, body: { servers, next_cursor: null } },
  [OPEN]: { status: 200, body: { incidents, next_cursor: null } },
});

const rows = async () => within(await screen.findByRole("table", { name: "Servers" })).getAllByRole("row").slice(1);

describe("FleetOverview", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl("/");
  });
  afterEach(() => vi.useRealTimers());

  it("lists servers problems first, with counters and a problem per row", async () => {
    stubFetch(reply([mail, old, db1, web1]));
    render(<FleetOverview />);
    const [first, second, third, fourth] = await rows();
    expect(first).toHaveTextContent(/web-1Debian 12 · agent 0\.1\.0 · via relay-paris-1\s*Online\s*Critical\s+Disk used \/var 96\.8% · 4 h 20 min\s*paris\s*12 s ago/);
    expect(second).toHaveTextContent(/db-1RHEL 9 · agent unknown\s*Offline\s*No heartbeat since 13:51/);
    expect(third).toHaveTextContent(/mail-1.*Online\s*Warning\s+Certificate expires in 6 days/);
    expect(fourth).toHaveTextContent(/web-10.*Revoked\s*Revoked on 14 Oct/);
    expect(within(first!).getByRole("link", { name: "web-1" })).toHaveAttribute("href", expect.stringMatching(/^\/servers\/view\/?\?id=s1$/));
    expect(screen.getByText(/4 servers, 2 online · Updated 14:32:05, refreshes every 15 seconds/)).toBeInTheDocument();

    const tiles = screen.getByRole("group", { name: "Summary, click to filter" });
    expect(within(tiles).getByRole("link", { name: /Online\s*2\s*\/ 4/ })).toHaveAttribute("href", "/?status=online");
    expect(within(tiles).getByRole("link", { name: /Critical incidents\s*1/ })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/incidents\/?\?state=open&severity=critical$/),
    );
    expect(within(tiles).getByRole("link", { name: /Warnings\s*0/ })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Problems first" })).toBeInTheDocument();
  });

  it("counts offline servers and relays in the critical tile", async () => {
    const opened = new Date(NOW.getTime() - 12 * 60_000).toISOString();
    const down = incident({ id: "i2", server_id: "s2", metric: "offline", instance: "", opened_at: opened });
    const relay = incident({ id: "i3", server_id: null, relay_id: "r1", hostname: "relay-1", customer_id: "c1", metric: "offline", instance: "" });
    stubFetch(reply([web1, db1], [down, relay]));
    render(<FleetOverview />);
    const [first] = await rows();
    expect(first).toHaveTextContent(/db-1.*Offline\s*Critical\s+Offline 12 min/);
    const tiles = screen.getByRole("group", { name: "Summary, click to filter" });
    expect(within(tiles).getByRole("link", { name: /Critical incidents\s*2/ })).toBeInTheDocument();
  });

  it("filters from the URL and keeps the scope", async () => {
    setUrl("/?customer=0b8f3c4e-1a2b-4c3d-8e9f-000000000001&status=offline");
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

  it("folds healthy servers after the first few", async () => {
    const healthy = Array.from({ length: HEALTHY_SHOWN + 2 }, (_, i) => server({ id: `h${i}`, hostname: `ok-${i + 1}` }));
    stubFetch(reply([web1, ...healthy]));
    render(<FleetOverview />);
    expect(await rows()).toHaveLength(1 + HEALTHY_SHOWN);
    expect(screen.getByText(/6 of 8 shown/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Show 2 more healthy servers" }));
    expect(await rows()).toHaveLength(8);
    expect(screen.getByRole("button", { name: "Fold healthy servers" })).toBeInTheDocument();

    // A sort shows every row, in the order asked.
    await userEvent.click(screen.getByRole("button", { name: "Host" }));
    expect(window.location.search).toBe("?sort=host");
    expect((await rows())[0]).toHaveTextContent("ok-1");
    await userEvent.click(screen.getByRole("button", { name: "Problems first" }));
    expect(window.location.search).toBe("");
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
    expect(screen.getAllByRole("link", { name: "Add servers" })[0]).toHaveAttribute("href", expect.stringMatching(/^\/admin\/tokens\/?$/));
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
