import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { type Command, type EffectiveSensors, type ServerDetail, setUnauthorizedHandler } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { COMMANDS_PAGE, ServerPage } from "./ServerPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
const NOW = new Date("2026-10-22T14:32:05");

const inDays = (days: number) => new Date(NOW.getTime() + days * 86_400_000).toISOString();

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
  created_at: "2026-05-03T09:12:00",
  revoked_at: null,
  status: "online",
  connected_at: "2026-10-19T02:14:00",
  last_heartbeat: new Date(NOW.getTime() - 12_000).toISOString(),
  via_relay: "relay-paris-1",
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

function config(overrides: Partial<EffectiveSensors> = {}): EffectiveSensors {
  return {
    server_id: ID,
    site_id: "site-1",
    revision: 7,
    applied_revision: 7,
    applied_at: new Date(NOW.getTime() - 180_000).toISOString(),
    in_sync: true,
    sensors: [
      { origin: "default", entry_id: null, kind: "cpu", name: "cpu", instance: "", interval_ms: 15_000, timeout_ms: 5_000, params: {} },
      { origin: "server", entry_id: "e1", kind: "disk", name: "disk", instance: "", interval_ms: 30_000, timeout_ms: 10_000, params: {} },
    ],
    ...overrides,
  };
}

const command = (overrides: Partial<Command>): Command => ({
  id: "c1",
  server_id: ID,
  kind: "apply_updates",
  parameters: { packages: [], security_only: true, allow_reboot: false },
  state: "running",
  issued_at: new Date(2026, 9, 22, 14, 20).toISOString(),
  expires_at: new Date(2026, 9, 22, 15, 20).toISOString(),
  sent_at: new Date(2026, 9, 22, 14, 20, 5).toISOString(),
  started_at: new Date(2026, 9, 22, 14, 20, 6).toISOString(),
  finished_at: null,
  exit_code: null,
  error: null,
  requested_by: "u1",
  campaign_id: null,
  ...overrides,
});

const failed = command({
  id: "c2",
  parameters: { packages: ["nginx"], security_only: false, allow_reboot: false },
  state: "failed",
  issued_at: new Date(2026, 9, 15, 9, 12).toISOString(),
  finished_at: new Date(2026, 9, 15, 9, 12, 19).toISOString(),
  exit_code: 100,
  requested_by: "u2",
});

const ok = (body: unknown) => ({ status: 200, body });
const value = (instance: string, v: number) => ({ labels: instance ? { instance } : {}, points: [[NOW.getTime() / 1000, v]] });

/** The page's requests; `overrides` replaces some. */
function routes(overrides: Record<string, unknown> = {}) {
  return {
    [`GET /api/servers/${ID}`]: ok(web1),
    [`GET /api/servers/${ID}/sensors`]: ok(config()),
    [`GET /api/metrics/query?server_id=${ID}&metric=monitorflow_cpu_usage_ratio&instance=total`]: ok({ query: "", series: [value("total", 0.384)] }),
    [`GET /api/metrics/query?server_id=${ID}&metric=monitorflow_disk_used_ratio`]: ok({ query: "", series: [value("/", 0.61), value("/var", 0.94)] }),
    [`GET /api/incidents?state=open&server_id=${ID}&limit=500`]: ok({
      incidents: [incident({ server_id: ID, opened_at: new Date(2026, 9, 22, 11, 40).toISOString(), last_value: 0.942 })],
      next_cursor: null,
    }),
    [`GET /api/servers/${ID}/commands?limit=${COMMANDS_PAGE}`]: ok({ commands: [command({}), failed], next_cursor: null }),
    "GET /api/agent-releases": ok({ versions: ["0.2.0"], latest: "0.2.0" }),
    ...overrides,
  } as Parameters<typeof stubFetch>[0];
}

const heading = () => screen.findByRole("heading", { level: 1, name: "web-1" });

describe("ServerPage", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl(`/servers/view/?id=${ID}`);
  });
  afterEach(() => vi.useRealTimers());

  it("shows the facts and the sensors, problems first", async () => {
    stubFetch(routes());
    render(<ServerPage id={ID} />);
    expect(screen.getByRole("status", { name: "Loading server" })).toBeInTheDocument();
    await heading();
    await vi.waitFor(() => expect(document.title).toBe("web-1 · MonitorFlow"));
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Fleet/web-1");
    const facts = document.querySelector("dl")!;
    expect(facts).toHaveTextContent(/Site\s*paris · acme/);
    expect(facts).toHaveTextContent(/Last seen\s*12 s ago/);
    expect(facts).toHaveTextContent(/OS\s*Debian 12/);
    expect(facts).toHaveTextContent(/Connected since\s*2026-10-19 02:14/);
    expect(facts).toHaveTextContent(/Agent\s*0\.2\.0 · via relay-paris-1/);
    expect(facts).toHaveTextContent(/Certificate\s*Valid until 2026-12/);
    expect(facts).toHaveTextContent(/Enrolled\s*2026-05-03 09:12/);
    expect(facts).toHaveTextContent(/Certificate ID\s*agent-1/);

    expect(screen.getByRole("tab", { name: "Sensors · 3" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "Commands · 1 running" })).toBeInTheDocument();
    expect(screen.getByText("In sync").parentElement).toHaveTextContent(/the agent runs revision 7, applied 3 min ago/);
    const rows = within(screen.getByRole("table", { name: "Sensors" })).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent)).toEqual([
      "Disk used /vardisk · /var · every 30 s · timeout 10 sCritical94.2% · since 11:40, 2 h 52 min",
      "CPU usagecpu · every 15 s · timeout 5 sOK38.4%",
      "Disk used /disk · / · every 30 s · timeout 10 sOK61%",
    ]);
    expect(within(rows[0]!).getByRole("link", { name: "Disk used /var" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/sensor/?\\?id=${ID}&sensor=disk_used_ratio&instance=%2Fvar$`)),
    );
  });

  it("shows configuration drift and missing sensors", async () => {
    stubFetch(routes({ [`GET /api/servers/${ID}/sensors`]: ok(config({ applied_revision: 6, in_sync: false })) }));
    const { unmount } = render(<ServerPage id={ID} />);
    expect(await screen.findByText("Configuration out of date")).toBeInTheDocument();
    expect(screen.getByText(/the agent runs revision 6, the current one is 7/)).toBeInTheDocument();
    unmount();

    stubFetch(routes({ [`GET /api/servers/${ID}/sensors`]: ok(config({ applied_revision: null, applied_at: null, in_sync: false, sensors: [] })) }));
    const again = render(<ServerPage id={ID} />);
    expect(await screen.findByText("Not applied")).toBeInTheDocument();
    expect(screen.getByText(/No sensors: every default is turned off/)).toBeInTheDocument();
    again.unmount();

    stubFetch(routes({ [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } } }));
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(/sensor configuration.*could not handle the request/);
    expect(screen.getByRole("heading", { level: 1, name: "web-1" })).toBeInTheDocument();
  });

  it("says why a sensor has no value", async () => {
    stubFetch(
      routes({
        [`GET /api/metrics/query?server_id=${ID}&metric=monitorflow_cpu_usage_ratio&instance=total`]: { status: 502, body: { error: "bad gateway" } },
      }),
    );
    render(<ServerPage id={ID} />);
    const table = await screen.findByRole("table", { name: "Sensors" });
    expect(within(table).getByRole("row", { name: /CPU usage/ })).toHaveTextContent(/Unknown\s*The metrics backend is unavailable/);
  });

  it("lists the commands in their tab, with the output under the row", async () => {
    setUrl(`/servers/view/?id=${ID}&tab=commands`);
    stubFetch(
      routes({
        "GET /api/commands/c2": ok({ ...failed, stdout: "Reading package lists...\n", stderr: "E: Could not get lock\n" }),
      }),
    );
    render(
      <As user={me({ role: "viewer", customer_id: null, site_id: null })}>
        <ServerPage id={ID} />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Commands" });
    expect(screen.getByRole("tab", { name: "Commands · 1 running" })).toHaveAttribute("aria-selected", "true");
    const tiles = screen.getByRole("group", { name: "Summary, click to filter" });
    expect(tiles).toHaveTextContent(/Running1\s*Queued0\s*Succeeded, 30 d0\s*Failed, 30 d1/);
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent(/Security updates, no rebootevery security update\s*Running 14:20:06\s*—\s*14:20 today\s*alice/);
    expect(rows[1]).toHaveTextContent(/Selected: nginx1 package\s*Failed 09:12:19\s*100\s*15 Oct 09:12\s*another user/);

    await userEvent.click(within(rows[1]!).getByRole("button", { name: "Log" }));
    const output = await screen.findByLabelText("Output of Selected: nginx");
    await vi.waitFor(() => expect(output).toHaveTextContent(/queued 09:12:00 → .*failed 09:12:19 \(exit 100\)/));
    expect(output).toHaveTextContent("E: Could not get lock");
    await userEvent.click(within(rows[1]!).getByRole("button", { name: "Hide log" }));
    expect(screen.queryByLabelText("Output of Selected: nginx")).not.toBeInTheDocument();

    expect(within(tiles).getByRole("link", { name: /Failed, 30 d/ })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/view/?\\?id=${ID}&tab=commands&state=failed$`)),
    );
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "State" }), "failed");
    expect(window.location.search).toBe(`?id=${ID}&tab=commands&state=failed`);
    await vi.waitFor(() => expect(within(screen.getByRole("table", { name: "Commands" })).getAllByRole("row")).toHaveLength(2));
  });

  it("switches tabs through the URL", async () => {
    stubFetch(routes());
    render(<ServerPage id={ID} />);
    await heading();
    await userEvent.click(screen.getByRole("tab", { name: "Updates" }));
    expect(window.location.search).toBe(`?id=${ID}&tab=updates`);
    expect(await screen.findByText(/applied through update campaigns/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: /Sensors/ }));
    expect(window.location.search).toBe(`?id=${ID}`);
  });

  it("shows the alerts and checks tab, editable by operators of the site", async () => {
    setUrl(`/servers/view/?id=${ID}&tab=alerts`);
    stubFetch(
      routes({
        [`GET /api/servers/${ID}/thresholds/effective`]: ok({ server_id: ID, site_id: "site-1", revision: 1, thresholds: [] }),
        [`GET /api/servers/${ID}/sensor_configs`]: ok({ entries: [] }),
      }),
    );
    render(
      <As user={me({ role: "operator", site_id: "site-1" })}>
        <ServerPage id={ID} />
      </As>,
    );
    expect(await screen.findByRole("region", { name: "What is checked" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "paris" })).toHaveAttribute("href", expect.stringMatching(/^\/sites\/view\/?\?id=site-1$/));
    expect(screen.getByRole("button", { name: "Add a check" })).toBeInTheDocument();
  });

  it("lets administrators of the customer revoke the server after confirmation", async () => {
    // Other customers' administrators and viewers get no button.
    stubFetch(routes());
    const { unmount } = render(
      <As user={me({ role: "administrator", customer_id: "c2", site_id: null })}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    expect(screen.queryByRole("button", { name: /Revoke/ })).not.toBeInTheDocument();
    unmount();

    const fetchMock = stubFetch(
      routes({
        [`GET /api/servers/${ID}`]: [ok(web1), ok({ ...web1, revoked_at: "2026-10-22T10:00:00Z", status: "offline" })],
        [`POST /api/servers/${ID}/revoke`]: ok({
          revoked_at: "2026-10-22T10:00:00Z",
          reason: "key_compromise",
          serials: ["3f2a"],
          closed_streams: 1,
        }),
      }),
    );
    render(
      <As user={customerAdmin}>
        <ServerPage id={ID} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Revoke web-1…" }));
    expect(screen.getByRole("form", { name: "Revoke web-1?" })).toHaveTextContent("cannot be undone");
    const confirm = screen.getByRole("button", { name: "Revoke web-1" });
    expect(confirm).toBeDisabled();
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Reason" }), "key_compromise");
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/revoke"))).toBe(false);
    await userEvent.click(confirm);
    const revoke = fetchMock.mock.calls.find(([url]) => String(url).endsWith("/revoke"));
    expect(JSON.parse(String(revoke?.[1]?.body))).toEqual({ reason: "key_compromise" });
    expect(await screen.findByText("Revoked", { selector: "span" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Revoke/ })).not.toBeInTheDocument();
  });

  it("says when the server does not exist or is out of scope", async () => {
    stubFetch(routes({ [`GET /api/servers/${ID}`]: { status: 404, body: { error: "not found" } } }));
    render(<ServerPage id={ID} />);
    expect(await screen.findByText("Server not found")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Back to the fleet" })).toHaveAttribute("href", "/");
  });

  it("does not call the API without a valid ID", () => {
    const fetchMock = stubFetch({});
    render(<ServerPage id="../../api/auth/logout" />);
    expect(screen.getByText("Server not found")).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows backend errors", async () => {
    stubFetch(routes({ [`GET /api/servers/${ID}`]: { status: 500, body: { error: "internal error" } } }));
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
  });

  it("leaves a 401 to the login redirect", async () => {
    stubFetch(routes({ [`GET /api/servers/${ID}`]: { status: 401, body: { error: "authentication required" } } }));
    const toLogin = vi.fn();
    const restore = setUnauthorizedHandler(toLogin);
    render(<ServerPage id={ID} />);
    await vi.waitFor(() => expect(toLogin).toHaveBeenCalled());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    restore();
  });

  it("warns about an expiring or revoked certificate", async () => {
    const certificate = { ...web1.certificate!, not_after: inDays(3.5) };
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok({ ...web1, certificate }) }));
    const { unmount } = render(<ServerPage id={ID} />);
    await heading();
    expect(screen.getByText("Expires in 4 days")).toBeInTheDocument();
    unmount();

    const revoked = { ...certificate, revoked: true, revoked_at: "2026-10-19T08:00:00", revocation_reason: "key_compromise" };
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok({ ...web1, certificate: revoked }) }));
    render(<ServerPage id={ID} />);
    await heading();
    expect(screen.getByText("Revoked").closest("dd")).toHaveTextContent("Revoked 2026-10-19 08:00 (key compromise)");
  });

  it("reads the server from its own endpoint, not the list", async () => {
    const fetchMock = stubFetch(routes());
    render(<ServerPage id={ID} />);
    await heading();
    const paths = fetchMock.mock.calls.map(([input]) => String(input));
    expect(paths).toContain(`/api/servers/${ID}`);
    expect(paths).not.toContain("/api/servers");
  });

  it("offers administrators an agent update when the server holds a newer release", async () => {
    const queued = command({ id: "c9", kind: "self_update", parameters: { version: "0.3.0" }, state: "queued" });
    const fetchMock = stubFetch(
      routes({
        "GET /api/agent-releases": ok({ versions: ["0.3.0", "0.2.0"], latest: "0.3.0" }),
        [`POST /api/servers/${ID}/commands`]: { status: 201, body: { ...queued, stdout: null, stderr: null } },
      }),
    );
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <As user={customerAdmin}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    expect(document.querySelector("dl")).toHaveTextContent(/Agent\s*0\.2\.0 · 0\.3\.0 available · via relay-paris-1/);
    await user.click(screen.getByRole("button", { name: "Update agent to 0.3.0…" }));
    await user.click(screen.getByRole("button", { name: "Update web-1" }));
    await vi.waitFor(() =>
      expect(fetchMock).toHaveBeenCalledWith(
        `/api/servers/${ID}/commands`,
        expect.objectContaining({ method: "POST", body: JSON.stringify({ kind: "self_update", version: "0.3.0" }) }),
      ),
    );
  });

  it("offers no agent update to viewers or up-to-date agents", async () => {
    stubFetch(routes({ "GET /api/agent-releases": ok({ versions: ["0.3.0"], latest: "0.3.0" }) }));
    const { unmount } = render(
      <As user={me({ role: "viewer", customer_id: null, site_id: null })}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    expect(screen.queryByRole("button", { name: /Update agent/ })).not.toBeInTheDocument();
    unmount();

    stubFetch(routes());
    render(
      <As user={customerAdmin}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    expect(screen.queryByRole("button", { name: /Update agent/ })).not.toBeInTheDocument();
    expect(document.querySelector("dl")).toHaveTextContent(/Agent\s*0\.2\.0 · via relay-paris-1/);
  });
});
