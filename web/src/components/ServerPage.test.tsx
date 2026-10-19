import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { type Server, setUnauthorizedHandler } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { ServerPage } from "./ServerPage";

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";

const web1: Server = {
  id: ID,
  peer_id: "agent-1",
  hostname: "web-1",
  site_id: "site-1",
  site_name: "paris",
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
};

const noThresholds = { status: 200, body: { server_id: ID, site_id: "site-1", revision: 1, thresholds: [] } };

describe("ServerPage", () => {
  it("shows the server header and its graphs", async () => {
    stubFetch({
      "GET /api/servers": { status: 200, body: { servers: [web1], next_cursor: null } },
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: {
        status: 200,
        body: { revision: 1, applied_revision: 1, applied_at: null, in_sync: true, sensors: [] },
      },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
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
    expect(document.title).toBe("web-1 · MonitorFlow");
    expect(await screen.findByRole("heading", { level: 2, name: "Metrics" })).toBeInTheDocument();
    expect(await screen.findByText(/No metrics yet/)).toBeInTheDocument();
    expect(await screen.findByText(/In sync/)).toBeInTheDocument();
  });

  it("draws the effective thresholds on the graphs", async () => {
    const disk = "monitorflow_disk_used_ratio";
    const end = Math.floor(Date.now() / 30_000) * 30;
    stubFetch({
      "GET /api/servers": { status: 200, body: { servers: [web1], next_cursor: null } },
      [`GET /api/servers/${ID}/metrics`]: {
        status: 200,
        body: { metrics: [{ name: disk, instances: ["/", "/boot"] }], truncated: false },
      },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
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
      "GET /api/servers": { status: 200, body: { servers: [web1], next_cursor: null } },
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
      [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: { status: 500, body: { error: "internal error" } },
    });
    render(<ServerPage id={ID} />);
    expect(await screen.findByText(/Threshold lines are not shown/)).toHaveTextContent("could not handle");
    expect(await screen.findByText(/No metrics yet/)).toBeInTheDocument();
  });

  it("says when the server does not exist or is out of scope", async () => {
    stubFetch({
      "GET /api/servers": { status: 200, body: { servers: [], next_cursor: null } },
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
      "GET /api/servers": { status: 500, body: { error: "internal error" } },
      [`GET /api/servers/${ID}/thresholds/effective`]: noThresholds,
    });
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
  });

  it("leaves a 401 to the login redirect", async () => {
    const unauthorized = { status: 401, body: { error: "authentication required" } };
    const fetchMock = stubFetch({
      "GET /api/servers": unauthorized,
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
});
