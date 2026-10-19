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

describe("ServerPage", () => {
  it("shows the server header and its graphs", async () => {
    stubFetch({
      "GET /api/servers": { status: 200, body: { servers: [web1], next_cursor: null } },
      [`GET /api/servers/${ID}/metrics`]: { status: 200, body: { metrics: [], truncated: false } },
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
  });

  it("says when the server does not exist or is out of scope", async () => {
    stubFetch({ "GET /api/servers": { status: 200, body: { servers: [], next_cursor: null } } });
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
    stubFetch({ "GET /api/servers": { status: 500, body: { error: "internal error" } } });
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
  });

  it("leaves a 401 to the login redirect", async () => {
    const fetchMock = stubFetch({ "GET /api/servers": { status: 401, body: { error: "authentication required" } } });
    const toLogin = vi.fn();
    const restore = setUnauthorizedHandler(toLogin);
    render(<ServerPage id={ID} />);
    await vi.waitFor(() => expect(toLogin).toHaveBeenCalledOnce());
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    restore();
  });
});
