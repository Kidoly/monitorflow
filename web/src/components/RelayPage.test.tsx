import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { CSRF_HEADER, type Incident, type RelayDetail, type Server } from "@/lib/api";
import { As, customerAdmin, relay, siteAdmin } from "@/test/admin";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { RelayPage } from "./RelayPage";

const WEB1 = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a03";
const DB1 = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a04";

const detail = (overrides: Partial<RelayDetail> = {}): RelayDetail => ({
  ...relay(),
  certificate: {
    serial: "0a1b2c",
    not_before: "2026-10-18T10:00:00Z",
    not_after: relay().cert_not_after!,
    issued_at: "2026-10-18T10:00:00Z",
    revoked: false,
    revoked_at: null,
    revocation_reason: null,
  },
  servers: [
    { id: WEB1, peer_id: "web-1", hostname: "web-1", status: "online" },
    { id: DB1, peer_id: "db-1", hostname: "db-1", status: "offline" },
  ],
  ...overrides,
});
const ID = relay().id;

const server = (id: string, hostname: string, overrides: Partial<Server> = {}): Server => ({
  id,
  peer_id: hostname,
  hostname,
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  relay_id: ID,
  os_family: "debian",
  os_version: "12",
  agent_version: "0.4.2",
  created_at: "2026-10-18T10:00:00Z",
  revoked_at: null,
  status: "online",
  connected_at: null,
  last_heartbeat: new Date().toISOString(),
  via_relay: "relay-1",
  open_incidents: 0,
  cert_not_after: null,
  ...overrides,
});

const incident = (overrides: Partial<Incident>): Incident => ({
  id: "i1",
  server_id: WEB1,
  hostname: "web-1",
  site_id: "site-1",
  site_name: "paris",
  metric: "monitorflow_cpu_usage_ratio",
  instance: "",
  severity: "warning",
  state: "open",
  opened_at: "2026-10-22T10:00:00Z",
  resolved_at: null,
  last_value: 0.92,
  last_value_at: "2026-10-22T10:00:00Z",
  peak_value: 0.95,
  threshold: {
    metric: "monitorflow_cpu_usage_ratio",
    instance: null,
    comparison: ">",
    warning: 0.8,
    critical: 0.95,
    duration_s: 300,
    hysteresis: 0,
  },
  ...overrides,
});

const lists = {
  "GET /api/servers": {
    status: 200,
    body: {
      servers: [
        server(WEB1, "web-1"),
        server(DB1, "db-1", { status: "offline", agent_version: "0.4.1", last_heartbeat: new Date(2026, 9, 20, 22, 5).toISOString() }),
      ],
      next_cursor: null,
    },
  },
  "GET /api/incidents?state=open&limit=500": {
    status: 200,
    body: {
      incidents: [incident({}), incident({ id: "i2", severity: "critical" }), incident({ id: "i3", server_id: "other" })],
      next_cursor: null,
    },
  },
};

const agentInstall = {
  source: "github",
  custom_url: null,
  source_from_config: false,
  custom_url_from_config: false,
  github_url: "https://github.com/x/releases/latest/download",
  central: null,
};
const publicAddress = {
  address: "mf.example",
  default: "mf.example",
  names: ["mf.example"],
  certified: true,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
};

describe("RelayPage", () => {
  it("shows the facts and the servers connecting through the relay", async () => {
    stubFetch({ [`GET /api/relays/${ID}`]: { status: 200, body: detail() }, ...lists });
    render(<RelayPage id={ID} me={siteAdmin} />);
    expect(await screen.findByRole("heading", { level: 1, name: "relay-1" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Relays/relay-1");
    expect(screen.getByText("Expires in 5 days")).toBeInTheDocument();
    expect(screen.getByText("Site").nextElementSibling).toHaveTextContent("paris · acme");
    expect(screen.getByText("Certificate serial").nextElementSibling).toHaveTextContent("0a1b2c");

    const attached = screen.getByRole("region", { name: "Servers connecting through relay-1" });
    expect(attached).toHaveTextContent("2 servers, 1 connected now");
    expect(within(attached).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/view/?\\?id=${WEB1}$`)),
    );
    const [, web, db] = within(attached).getAllByRole("row");
    // The worst open incident of the server, with how many more are open.
    expect(web).toHaveTextContent(/web-1\s*Debian 12 · agent 0\.4\.2\s*Online\s*Critical\s+CPU usage\s*92%\s*· 1 more/);
    expect(db).toHaveTextContent(/db-1\s*Debian 12 · agent 0\.4\.1\s*Offline\s*No heartbeat since 2026-10-20 22:05/);
    // A site administrator cannot remove.
    expect(screen.queryByRole("button", { name: /Remove/ })).not.toBeInTheDocument();
  });

  it("removes after confirmation, then shows the uninstall command", async () => {
    const fetchMock = stubFetch({
      [`GET /api/relays/${ID}`]: [
        { status: 200, body: detail() },
        { status: 200, body: detail({ revoked_at: "2026-10-22T10:00:00Z", status: "offline", servers: [] }) },
      ],
      [`POST /api/relays/${ID}/revoke`]: {
        status: 200,
        body: { revoked_at: "2026-10-22T10:00:00Z", reason: "cessation_of_operation", serials: ["0a"], closed_streams: 2 },
      },
      "GET /api/settings/agent-install": { status: 200, body: agentInstall },
      "GET /api/settings/public-address": { status: 200, body: publicAddress },
      ...lists,
    });
    render(
      <As user={customerAdmin}>
        <RelayPage id={ID} me={customerAdmin} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Remove relay-1…" }));
    const form = screen.getByRole("form", { name: "Remove relay-1?" });
    expect(form).toHaveTextContent("with the agents it forwards");
    await userEvent.click(within(form).getByRole("button", { name: "Cancel" }));
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/revoke"))).toBe(false);

    await userEvent.click(screen.getByRole("button", { name: "Remove relay-1…" }));
    await userEvent.click(screen.getByRole("button", { name: "Remove relay-1" }));
    expect(await screen.findByText("Removed", { selector: "span" })).toBeInTheDocument();
    const call = fetchMock.mock.calls.findIndex(([url]) => String(url).endsWith("/revoke"));
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
    expect(fetchMock.mock.calls[call]?.[1]?.body).toBe(JSON.stringify({ reason: "cessation_of_operation" }));
    expect(screen.queryByRole("button", { name: /Remove/ })).not.toBeInTheDocument();
    expect(await screen.findByText(/MF_ROLE=relay MF_UNINSTALL=1 MF_PURGE=1/)).toBeInTheDocument();
  });

  it("updates an older relay to the newest release, after confirmation", async () => {
    const queued = {
      id: "rc1",
      relay_id: ID,
      version: "0.3.0",
      state: "queued" as const,
      requested_by: "u1",
      issued_at: "2026-10-27T10:00:00Z",
      expires_at: "2026-10-28T10:00:00Z",
      finished_at: null,
      stdout: null,
      error: null,
    };
    const fetchMock = stubFetch({
      [`GET /api/relays/${ID}`]: [
        { status: 200, body: detail({ relay_version: "0.2.0" }) },
        { status: 200, body: detail({ relay_version: "0.2.0", last_update: queued, updates: [queued] }) },
      ],
      "GET /api/agent-releases": { status: 200, body: { latest: "0.3.0", versions: ["0.3.0", "0.2.0"] } },
      [`POST /api/relays/${ID}/update`]: { status: 202, body: queued },
      ...lists,
    });
    render(
      <As user={customerAdmin}>
        <RelayPage id={ID} me={customerAdmin} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Update relay to 0.3.0…" }));
    expect(screen.getByRole("form", { name: "Update relay-1 to 0.3.0?" })).toHaveTextContent("agents reconnect");
    await userEvent.click(screen.getByRole("button", { name: "Update relay-1" }));
    expect(await screen.findByText("Update to 0.3.0 queued")).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url]) => String(url).endsWith("/update"));
    expect(call?.[1]?.body).toBe(JSON.stringify({ version: "0.3.0" }));
    expect(screen.queryByRole("button", { name: /Update relay/ })).not.toBeInTheDocument();
  });

  it("says when the relay is not found", async () => {
    stubFetch({ [`GET /api/relays/${ID}`]: { status: 404, body: { error: "not found" } } });
    render(<RelayPage id={ID} me={customerAdmin} />);
    expect(await screen.findByText("Relay not found")).toBeInTheDocument();
  });
});
