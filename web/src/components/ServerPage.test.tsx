import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { type ServerDetail, setUnauthorizedHandler } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";

import { ServerPage } from "./ServerPage";

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";

const inDays = (days: number) => new Date(Date.now() + days * 86_400_000).toISOString();

const web1: ServerDetail = {
  id: ID,
  peer_id: "agent-1",
  hostname: "web-1",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  relay_id: null,
  os_family: "debian",
  os_version: "12",
  agent_version: "0.2.0",
  created_at: "2026-10-18T10:00:00Z",
  revoked_at: null,
  status: "online",
  connected_at: new Date().toISOString(),
  last_heartbeat: new Date().toISOString(),
  via_relay: null,
  open_incidents: 0,
  cert_not_after: inDays(60.5),
  certificate: {
    serial: "3f2a",
    not_before: "2026-10-18T10:00:00Z",
    not_after: inDays(60.5),
    issued_at: "2026-10-18T10:00:00Z",
    revoked: false,
    revoked_at: null,
    revocation_reason: null,
  },
};

const detail = (body: ServerDetail = web1) => ({ [`GET /api/servers/${ID}`]: { status: 200, body } });

const openIncidents = `GET /api/incidents?state=open&server_id=${ID}&limit=10`;
const noIncidents = { status: 200, body: { incidents: [], next_cursor: null } };
const noThresholds = { status: 200, body: { server_id: ID, site_id: "site-1", revision: 1, thresholds: [] } };

describe("ServerPage", () => {
  it("shows the server header and its graphs", async () => {
    stubFetch({
      ...detail(),
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: {
        status: 200,
        body: { revision: 1, applied_revision: 1, applied_at: null, in_sync: true, sensors: [] },
      },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
      [openIncidents]: {
        status: 200,
        body: { incidents: [incident({ server_id: ID })], next_cursor: null },
      },
    });
    render(<ServerPage id={ID} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading server…");
    expect(await screen.findByRole("heading", { level: 1, name: "web-1" })).toBeInTheDocument();
    const header = screen.getByRole("banner");
    expect(header).toHaveTextContent("Online");
    expect(header).toHaveTextContent(/Site\s*paris/);
    expect(header).toHaveTextContent(/OS\s*Debian 12/);
    expect(header).toHaveTextContent(/Agent\s*0\.2\.0/);
    expect(header).toHaveTextContent(/Last seen\s*just now/);
    expect(header).toHaveTextContent(/Certificate ID\s*agent-1/);
    expect(header).toHaveTextContent(/Certificate expires\s*in 60 days/);
    expect(header).not.toHaveTextContent(/⚠|Certificate revoked/);
    expect(document.title).toBe("web-1 · MonitorFlow");
    expect(await screen.findByRole("heading", { level: 2, name: "Metrics" })).toBeInTheDocument();
    expect(await screen.findByText(/No metrics yet/)).toBeInTheDocument();
    expect(await screen.findByText(/In sync/)).toBeInTheDocument();
    const incidents = screen.getByRole("region", { name: "Open incidents" });
    const link = await within(incidents).findByRole("link", { name: "Disk used /var" });
    expect(link).toHaveAttribute("href", expect.stringMatching(/^\/incidents\/view\/?\?id=1d6a9a3e-/));
    expect(within(incidents).getByRole("listitem")).toHaveTextContent(/◆ Critical\s*Disk used \/var\s*96\.2%/);
    expect(within(incidents).getByRole("link", { name: "Incident history" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/incidents/?\\?server=${ID}$`)),
    );
  });

  it("draws the effective thresholds on the graphs", async () => {
    const disk = "monitorflow_disk_used_ratio";
    const end = Math.floor(Date.now() / 30_000) * 30;
    stubFetch({
      ...detail(),
      [`GET /api/servers/${ID}/metrics`]: {
        status: 200,
        body: { metrics: [{ name: disk, instances: ["/", "/boot"] }], truncated: false },
      },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [openIncidents]: noIncidents,
      [`GET /api/servers/${ID}/thresholds/effective`]: {
        status: 200,
        body: {
          ...noThresholds.body,
          thresholds: [
            { origin: "default", threshold_id: null, metric: disk, instance: null, enabled: true, comparison: ">", warning: 0.85, critical: 0.95, duration_s: 300, hysteresis: 0.02 },
            { origin: "server", threshold_id: "t1", metric: disk, instance: "/boot", enabled: false, comparison: ">", warning: 0.5, critical: null, duration_s: 0, hysteresis: 0 },
          ],
        },
      },
    });
    const fetchMock = vi.mocked(fetch);
    fetchMock.mockImplementation(
      ((original) => async (input, init) => {
        if (String(input).startsWith("/api/metrics/query_range")) {
          const series = ["/", "/boot"].map((instance) => ({ labels: { instance }, points: [[end, 0.4]] }));
          return new Response(JSON.stringify({ query: "…", series }), { status: 200 });
        }
        return original(input, init);
      })(fetchMock.getMockImplementation()!),
    );
    render(<ServerPage id={ID} />);
    const lines = await screen.findAllByTestId("reference-line");
    expect(lines.map((l) => l.textContent)).toEqual(["Warning (/) 85%", "Critical (/) 95%"]);
  });

  it("shows the graphs without lines when thresholds fail", async () => {
    stubFetch({
      ...detail(),
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [openIncidents]: noIncidents,
      [`GET /api/servers/${ID}/thresholds/effective`]: { status: 500, body: { error: "internal error" } },
    });
    render(<ServerPage id={ID} />);
    expect(await screen.findByText(/Threshold lines are not shown/)).toHaveTextContent("could not handle");
    expect(await screen.findByText(/No metrics yet/)).toBeInTheDocument();
  });

  it("lets administrators of the customer revoke the server after confirmation", async () => {
    const page = {
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [openIncidents]: noIncidents,
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
    };
    // Other customers' administrators and viewers get no button.
    stubFetch({ ...detail(), ...page });
    const { unmount } = render(
      <As user={me({ role: "administrator", customer_id: "c2", site_id: null })}>
        <ServerPage id={ID} />
      </As>,
    );
    await screen.findByRole("heading", { level: 1, name: "web-1" });
    expect(screen.queryByRole("button", { name: /Revoke/ })).not.toBeInTheDocument();
    unmount();

    const fetchMock = stubFetch({
      ...page,
      [`GET /api/servers/${ID}`]: [
        { status: 200, body: web1 },
        { status: 200, body: { ...web1, revoked_at: "2026-10-22T10:00:00Z", status: "offline" } },
      ],
      [`POST /api/servers/${ID}/revoke`]: {
        status: 200,
        body: { revoked_at: "2026-10-22T10:00:00Z", reason: "unspecified", serials: ["3f2a"], closed_streams: 1 },
      },
    });
    render(
      <As user={customerAdmin}>
        <ServerPage id={ID} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Revoke server…" }));
    expect(screen.getByRole("form", { name: "Revoke web-1" })).toHaveTextContent("cannot be undone");
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/revoke"))).toBe(false);
    await userEvent.click(screen.getByRole("button", { name: "Revoke web-1" }));
    expect(await screen.findByText(/^Revoked$/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Revoke/ })).not.toBeInTheDocument();
  });

  it("says when the server does not exist or is out of scope", async () => {
    stubFetch({
      [`GET /api/servers/${ID}`]: { status: 404, body: { error: "not found" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: { status: 404, body: { error: "not found" } },
    });
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("heading", { name: "Server not found" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Back to the fleet" })).toHaveAttribute("href", "/");
  });

  it("does not call the API without a valid ID", () => {
    const fetchMock = stubFetch({});
    render(<ServerPage id="../../api/auth/logout" />);
    expect(screen.getByRole("heading", { name: "Server not found" })).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows backend errors", async () => {
    stubFetch({
      [`GET /api/servers/${ID}`]: { status: 500, body: { error: "internal error" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
    });
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
  });

  it("leaves a 401 to the login redirect", async () => {
    const unauthorized = { status: 401, body: { error: "authentication required" } };
    const fetchMock = stubFetch({
      [`GET /api/servers/${ID}`]: unauthorized,
      [`GET /api/servers/${ID}/thresholds/effective`]: unauthorized,
    });
    const toLogin = vi.fn();
    const restore = setUnauthorizedHandler(toLogin);
    render(<ServerPage id={ID} />);
    await vi.waitFor(() => expect(toLogin).toHaveBeenCalled());
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    restore();
  });

  it("warns about an expiring or revoked certificate", async () => {
    const certificate = { ...web1.certificate!, not_after: inDays(3.5) };
    stubFetch({
      ...detail({ ...web1, certificate }),
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
      [openIncidents]: noIncidents,
    });
    const { unmount } = render(<ServerPage id={ID} />);
    const header = await screen.findByRole("banner");
    expect(header).toHaveTextContent(/Certificate expires\s*⚠ Expires in 3 days/);
    unmount();

    const revoked = {
      ...certificate,
      not_after: inDays(-2.5),
      revoked: true,
      revoked_at: "2026-10-19T08:00:00Z",
      revocation_reason: "key compromise",
    };
    stubFetch({
      ...detail({ ...web1, certificate: revoked }),
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
      [openIncidents]: noIncidents,
    });
    render(<ServerPage id={ID} />);
    const again = await screen.findByRole("banner");
    expect(again).toHaveTextContent(/⚠ Expired 2 days ago/);
    expect(again).toHaveTextContent(/Certificate revoked\s*.*2026.*\(key compromise\)/);
  });

  it("reads the server from its own endpoint, not the list", async () => {
    const fetchMock = stubFetch({
      ...detail(),
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
      [openIncidents]: noIncidents,
    });
    render(<ServerPage id={ID} />);
    await screen.findByRole("heading", { level: 1, name: "web-1" });
    const paths = fetchMock.mock.calls.map(([input]) => String(input));
    expect(paths).toContain(`/api/servers/${ID}`);
    expect(paths).not.toContain("/api/servers");
  });
});
