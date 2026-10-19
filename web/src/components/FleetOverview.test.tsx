import { act, render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Server } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { FleetOverview, REFRESH_MS } from "./FleetOverview";

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
    last_heartbeat: new Date().toISOString(),
    via_relay: null,
    open_incidents: 0,
    cert_not_after: new Date(Date.now() + 60 * 86_400_000).toISOString(),
    ...overrides,
  };
}

const servers = [
  server({ open_incidents: 2 }),
  server({
    id: "s2",
    hostname: "db-1",
    os_family: "rhel",
    os_version: "9",
    agent_version: null,
    status: "offline",
    connected_at: null,
    last_heartbeat: null,
  }),
];

describe("FleetOverview", () => {
  afterEach(() => vi.useRealTimers());

  it("lists servers with their status", async () => {
    stubFetch({ "GET /api/servers": { status: 200, body: { servers } } });
    render(<FleetOverview />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading servers…");

    const rows = within(await screen.findByRole("table", { name: "Fleet" })).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(/paris\s*web-1\s*Debian 12\s*0\.1\.0\s*● Online\s*2 open\s*just now/);
    expect(rows[2]).toHaveTextContent(/db-1\s*RHEL 9\s*unknown\s*○ Offline\s*None\s*never/);
    expect(within(rows[1]!).getByRole("link", { name: "2 open" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/incidents\/?\?state=open&server=s1$/),
    );
    expect(within(rows[2]!).queryByRole("link", { name: /open/ })).not.toBeInTheDocument();
    expect(screen.getByText(/2 servers, 1 online/)).toBeInTheDocument();
    expect(within(rows[1]!).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      // next/link adds the trailing slash only in a build with `trailingSlash`.
      expect.stringMatching(/^\/servers\/view\/?\?id=s1$/),
    );
  });

  it("warns about certificates close to expiry", async () => {
    const inDays = (days: number) => new Date(Date.now() + days * 86_400_000).toISOString();
    stubFetch({
      "GET /api/servers": {
        status: 200,
        body: {
          servers: [
            server({ id: "a", hostname: "a", cert_not_after: inDays(15) }),
            server({ id: "b", hostname: "b", cert_not_after: inDays(5.5) }),
            server({ id: "c", hostname: "c", cert_not_after: inDays(-2.5) }),
            server({ id: "d", hostname: "d", cert_not_after: null }),
          ],
        },
      },
    });
    render(<FleetOverview />);
    const rows = within(await screen.findByRole("table", { name: "Fleet" })).getAllByRole("row").slice(1);
    expect(rows[0]).not.toHaveTextContent(/Certificate/);
    expect(rows[1]).toHaveTextContent(/● Online\s*⚠ Certificate expires in 5 days/);
    expect(rows[2]).toHaveTextContent(/⚠ Certificate expired 2 days ago/);
    expect(rows[3]).not.toHaveTextContent(/Certificate/);
  });

  it("shows an empty state", async () => {
    stubFetch({ "GET /api/servers": { status: 200, body: { servers: [] } } });
    render(<FleetOverview />);
    expect(await screen.findByText(/No servers yet/)).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("explains a 403", async () => {
    stubFetch({ "GET /api/servers": { status: 403, body: { error: "forbidden" } } });
    render(<FleetOverview />);
    expect(await screen.findByRole("alert")).toHaveTextContent("You do not have access");
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
  });

  it("refreshes periodically and keeps the last data when a refresh fails", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const fetchMock = stubFetch({
      "GET /api/servers": [
        { status: 200, body: { servers: [servers[0]] } },
        { status: 200, body: { servers } },
        { status: 502, body: { error: "bad gateway" } },
      ],
    });
    render(<FleetOverview />);
    expect(await screen.findByText(/1 server, 1 online/)).toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByText(/2 servers, 1 online/)).toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByRole("alert")).toHaveTextContent(/Refresh failed.*last data/);
    expect(screen.getByRole("table")).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(3);
  });
});
