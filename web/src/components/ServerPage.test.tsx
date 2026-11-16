import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { type Command, type EffectiveSensors, type ServerDetail, setUnauthorizedHandler } from "@/lib/api";
import { As, customerAdmin, oneSite, me, single } from "@/test/admin";
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
    check_interval_ms: 60_000,
    check_interval_origin: "default",
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

/** The built-in disk and CPU alerts (ADR-0017). */
const THRESHOLDS = [
  { origin: "default", threshold_id: null, metric: "monitorflow_disk_used_ratio", instance: null, enabled: true, comparison: ">", warning: 0.85, critical: 0.95, duration_s: 300, hysteresis: 0.02 },
  { origin: "default", threshold_id: null, metric: "monitorflow_cpu_usage_ratio", instance: "total", enabled: true, comparison: ">", warning: 0.9, critical: null, duration_s: 600, hysteresis: 0.02 },
];
const T = NOW.getTime() / 1000;
const series = (instance: string, v: number, reason: string | null = null) => ({
  instance,
  points: [[T - 120, v], [T, v]],
  last_value: v,
  last_at: NOW.toISOString(),
  reason,
});
const values = (disk: unknown[] = [series("/", 0.61), series("/var", 0.94)]) =>
  ok({
    start: T - 3600,
    end: T,
    step: 120,
    sensors: [
      { name: "cpu", instance: "", metric: "monitorflow_cpu_usage_ratio", series: [series("total", 0.384)], reason: null },
      { name: "disk", instance: "", metric: "monitorflow_disk_used_ratio", series: disk, reason: null },
    ],
  });

/** The page's requests; `overrides` replaces some. */
function routes(overrides: Record<string, unknown> = {}) {
  return {
    [`GET /api/servers/${ID}`]: ok(web1),
    [`GET /api/servers/${ID}/sensors`]: ok(config()),
    [`GET /api/servers/${ID}/sensor-values`]: values(),
    [`GET /api/incidents?state=open&server_id=${ID}&limit=500`]: ok({
      incidents: [
        incident({ server_id: ID, opened_at: new Date(2026, 9, 22, 11, 40).toISOString(), last_value: 0.942, reason: "94.2% used is above the critical limit of 95%" }),
      ],
      next_cursor: null,
    }),
    [`GET /api/servers/${ID}/commands?limit=${COMMANDS_PAGE}`]: ok({ commands: [command({}), failed], next_cursor: null }),
    "GET /api/agent-releases": ok({ versions: ["0.2.0"], latest: "0.2.0" }),
    [`GET /api/servers/${ID}/thresholds/effective`]: ok({ server_id: ID, site_id: "site-1", revision: 1, thresholds: THRESHOLDS }),
    [`GET /api/servers/${ID}/sensor_configs`]: ok({ entries: [] }),
    "GET /api/sites/site-1/sensors": ok({ site_id: "site-1", check_interval_ms: 60_000, check_interval_origin: "default", sensors: [] }),
    [`GET /api/servers/${ID}/sources`]: ok({
      using: "own",
      mirrors: [],
      changing_to: null,
      failed: null,
      campaign: false,
      distribution: null,
      problem: "MonitorFlow has no mirror for debian 11. It keeps its own package sources.",
      add_mirror: false,
    }),
    ...overrides,
  } as Parameters<typeof stubFetch>[0];
}

/** The title block: title, status, actions, the facts line and its badges. */
const header = () => screen.getByRole("heading", { level: 1 }).parentElement!;

/** Waits for the page, then opens Details, where the full facts list is. */
async function heading() {
  const title = await screen.findByRole("heading", { level: 1, name: "web-1" });
  await userEvent.click(screen.getByRole("button", { name: /^(Details|Détails)$/ }));
  return title;
}

describe("ServerPage", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl(`/servers/view/?id=${ID}`);
  });
  afterEach(() => vi.useRealTimers());

  it("names the site alone for a single company or a customer's user (ADR-0104)", async () => {
    stubFetch(routes());
    render(
      <As user={single(customerAdmin)}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    // The header names the site; Details adds no customer.
    expect(screen.getByText(/^Debian 12 · agent 0\.2\.0/)).toHaveTextContent(/ · paris · /);
    expect(document.querySelector("dl")!).not.toHaveTextContent("acme");
    expect(document.querySelector("dl")!).not.toHaveTextContent(/Site|Customer/);
  });

  it("leaves to the header what it already says: a limited rollback, the certificate ID that is the host name", async () => {
    const limited = { available: false, reason: "disabled", detail: "/ is on plain ext4", local_reason: { code: "root_not_covered", fs_type: "ext4" } };
    const rollback = (snapshots: object) => ok({ snapshots, not_covered: [], local_snapshots: [] });
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok({ ...web1, peer_id: "web-1" }), [`GET /api/servers/${ID}/rollback`]: rollback(limited) }));
    const { unmount } = render(<ServerPage id={ID} />);
    await heading();
    await screen.findByText("Rollback limited");
    expect(document.querySelector("dl")!).not.toHaveTextContent(/Rollback|Certificate ID/);
    unmount();

    const lvm = { available: true, reason: "disabled", method: "lvm", detail: "LVM snapshot of /" };
    stubFetch(routes({ [`GET /api/servers/${ID}/rollback`]: rollback(lvm) }));
    render(<ServerPage id={ID} />);
    await heading();
    expect(await within(document.querySelector("dl")!).findByText("Rollback")).toBeInTheDocument();
  });

  it("names no site while there is only one", async () => {
    stubFetch(routes());
    render(
      <As user={oneSite(customerAdmin)}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    expect(screen.getByText(/^Debian 12 · agent 0\.2\.0/)).toHaveTextContent(/^Debian 12 · agent 0\.2\.0 · seen 12 s ago/);
    expect(screen.queryByRole("link", { name: "paris" })).toBeNull();
  });

  it("shows the facts and the sensors, problems first", async () => {
    stubFetch(routes());
    render(<ServerPage id={ID} />);
    expect(screen.getByRole("status", { name: "Loading server" })).toBeInTheDocument();
    await heading();
    await vi.waitFor(() => expect(document.title).toBe("web-1 · MonitorFlow"));
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent(/^Fleet›▾$/);
    const facts = document.querySelector("dl")!;
    // Only what the header line above does not say.
    expect(facts).toHaveTextContent(/Customer\s*acme/);
    expect(facts).not.toHaveTextContent(/Site|Last seen|OS|0\.2\.0/);
    expect(facts).toHaveTextContent(/Connected since\s*2026-10-19 02:14/);
    expect(facts).toHaveTextContent(/Agent\s*via relay-paris-1/);
    expect(facts).toHaveTextContent(/Certificate\s*Valid until 2026-12-\d\d \(renewed automatically\)/);
    expect(facts).toHaveTextContent(/Added\s*2026-05-03 09:12/);
    // Shown as it differs from the host name.
    expect(facts).toHaveTextContent(/Certificate ID\s*agent-1/);
    // One line of facts under the title, and no badge while everything is as usual.
    expect(screen.getByText(/^Debian 12 · agent 0\.2\.0/)).toHaveTextContent(/^Debian 12 · agent 0\.2\.0 · paris · seen 12 s ago/);
    expect(within(header()).queryByRole("tooltip", { hidden: true })).not.toBeInTheDocument();

    expect(screen.getByRole("tab", { name: "Sensors · 3" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "Commands · 1 running" })).toBeInTheDocument();
    expect(screen.queryByText(/Configuration up to date/)).not.toBeInTheDocument();
    const rows = within(screen.getByRole("table", { name: "Sensors" })).getAllByRole("row").slice(1);
    // Each row: the sensor (no "disk · /" under it, nor an incident's reason that the status, value and
    // rule beside it say), its status, its value alone, then its compact rule (the sentence in its
    // tooltip) and where the rule comes from.
    expect(rows.map((r) => r.textContent)).toEqual([
      "Disk used /varCritical94.2%30 s · > 85% / 95% · 5 minevery 30 s · warn > 85% · crit > 95% · for 5 minThis server⋯",
      "CPU usageOK38.4%15 s · > 90% · 10 minevery 15 s · warn > 90% · for 10 minBuilt-in⋯",
      "Disk used /OK61%30 s · > 85% / 95% · 5 minevery 30 s · warn > 85% · crit > 95% · for 5 minThis server⋯",
    ]);
    expect(within(rows[0]!).getByText("30 s · > 85% / 95% · 5 min").closest("[tabindex]")).toHaveAccessibleDescription(
      "every 30 s · warn > 85% · crit > 95% · for 5 min",
    );
    expect(screen.queryByRole("tab", { name: "Alerts and checks" })).not.toBeInTheDocument();
    expect(within(rows[1]!).getByRole("img", { name: "CPU usage, last hour" })).toBeInTheDocument();
    expect(within(rows[0]!).getByRole("link", { name: "Disk used /var" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/sensor/?\\?id=${ID}&sensor=disk_used_ratio&instance=%2Fvar$`)),
    );
  });

  it("speaks French when that is the language", async () => {
    setActiveLocale("fr");
    stubFetch(routes());
    render(<ServerPage id={ID} />);
    await heading();
    expect(screen.getByRole("navigation", { name: "Fil d’Ariane" })).toHaveTextContent(/^Parc›▾$/);
    const facts = document.querySelector("dl")!;
    expect(facts).toHaveTextContent(/Connecté depuis\s*2026-10-19 02:14/);
    expect(facts).toHaveTextContent(/Certificat\s*Valide jusqu’au 2026-12-\d\d \(renouvelé automatiquement\)/);
    expect(screen.getByRole("tab", { name: "Capteurs · 3" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "Commandes · 1 en cours" })).toBeInTheDocument();
    const table = screen.getByRole("table", { name: "Capteurs" });
    expect(within(table).getByRole("columnheader", { name: "Dernière valeur" })).toBeInTheDocument();
    expect(within(table).getByRole("img", { name: "Utilisation CPU, dernière heure" })).toBeInTheDocument();
  });

  it("shows configuration drift and missing sensors", async () => {
    stubFetch(routes({ [`GET /api/servers/${ID}/sensors`]: ok(config({ applied_revision: 6, in_sync: false })) }));
    const { unmount } = render(<ServerPage id={ID} />);
    expect(await screen.findByText("Configuration out of date")).toBeInTheDocument();
    expect(screen.getByText(/a connected agent gets the new one/)).toBeInTheDocument();
    unmount();

    stubFetch(routes({ [`GET /api/servers/${ID}/sensors`]: ok(config({ applied_revision: null, applied_at: null, in_sync: false, sensors: [] })) }));
    const again = render(<ServerPage id={ID} />);
    expect(await screen.findByText("Waiting for the agent")).toBeInTheDocument();
    expect(screen.getByText(/No sensors: every default is turned off/)).toBeInTheDocument();
    again.unmount();

    stubFetch(routes({ [`GET /api/servers/${ID}/sensors`]: { status: 500, body: { error: "internal error" } } }));
    render(<ServerPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent(/sensor configuration.*could not handle the request/);
    expect(screen.getByRole("heading", { level: 1, name: "web-1" })).toBeInTheDocument();
  });

  it("reads every sensor's values in one request and says when one is late", async () => {
    const fetchMock = stubFetch(routes({
        [`GET /api/servers/${ID}/sensor-values`]: values([series("/", 0.61, "No data for 15 minutes")]),
        [`GET /api/incidents?state=open&server_id=${ID}&limit=500`]: ok({ incidents: [], next_cursor: null }),
      }));
    render(<ServerPage id={ID} />);
    const table = await screen.findByRole("table", { name: "Sensors" });
    expect(within(table).getByRole("row", { name: /Disk used \// })).toHaveTextContent(/No data for 15 minutes\s*Unknown/);
    const urls = fetchMock.mock.calls.map(([input]) => String(input));
    expect(urls.filter((u) => u.startsWith("/api/metrics/"))).toEqual([]);
    expect(urls.filter((u) => u.endsWith("/sensor-values"))).toHaveLength(1);
  });

  it("says why a sensor has no value", async () => {
    stubFetch(
      routes({
        [`GET /api/servers/${ID}/sensor-values`]: { status: 502, body: { error: "bad gateway" } },
      }),
    );
    render(<ServerPage id={ID} />);
    const table = await screen.findByRole("table", { name: "Sensors" });
    expect(within(table).getByRole("row", { name: /CPU usage/ })).toHaveTextContent(/The metrics backend is unavailable.*Unknown/);
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
    expect(rows[0]).toHaveTextContent(/Security updates, no rebootevery security update\s*Running 14:20:06\s*14:20 today\s*alice/);
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

  it("says in plain words when the host takes no package updates", async () => {
    const server = { ...web1, os_family: "linux", os_version: "Alpine 3.20", package_updates: false };
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok(server) }));
    render(<ServerPage id={ID} />);
    await heading();
    expect(screen.getByText("No package updates")).toBeInTheDocument();
    const facts = within(document.querySelector("dl")!);
    expect(facts.getByText("Package updates aren't available on this system; monitoring works normally")).toBeInTheDocument();
    expect(screen.getByText(/^Alpine 3\.20 · agent/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("tab", { name: "Updates" }));
    expect(await screen.findByText(/update web-1 with\s+its own tools/)).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Open campaigns" })).not.toBeInTheDocument();
  });

  describe("AI commands", () => {
    const install = {
      "GET /api/settings/agent-install": ok({
        source: "github",
        custom_url: null,
        source_from_config: false,
        custom_url_from_config: false,
        github_url: "https://github.com/Kidoly/monitorflow/releases/latest/download",
        central: null,
      }),
      "GET /api/settings/public-address": ok({
        address: "mf.example",
        default: "mf.example",
        names: ["mf.example"],
        certified: true,
        upstream_url: "https://mf.example:7443",
        enroll_url: "https://mf.example:7445",
      }),
    };
    const off = { apply_updates: "accepted", configure_sources: "accepted", revert_sources: "accepted", self_update: "accepted", shell: "off" };
    const reporting = (command_kinds: ServerDetail["command_kinds"], more: Partial<ServerDetail> = {}) =>
      ok({ ...web1, command_kinds, ...more });

    it("is not known yet for an agent that does not report, whatever else it says", async () => {
      stubFetch(routes({ [`GET /api/servers/${ID}`]: ok(web1) }));
      render(<ServerPage id={ID} />);
      await heading();
      expect(screen.getByText("Not known yet")).toBeInTheDocument();
      expect(screen.getByText(/The agent and its relay will say once updated/)).toBeInTheDocument();
      expect(screen.queryByText(/Runs the commands AI sends/)).not.toBeInTheDocument();
      expect(screen.queryByText(/Refuses AI/)).not.toBeInTheDocument();
    });

    it("says it is on", async () => {
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting({ ...off, shell: "accepted" }) }));
      render(<ServerPage id={ID} />);
      await heading();
      expect(screen.getByText("AI commands on")).toBeInTheDocument();
      // The label says it; no sentence repeating it, and no AI line among the other commands.
      expect(document.querySelector("dl")!).toHaveTextContent(/AI commands\s*On/);
      expect(document.querySelector("dl")!).not.toHaveTextContent(/Runs the commands AI sends/);
      expect(screen.queryByRole("link", { name: "How to turn them on" })).not.toBeInTheDocument();
    });

    it("says it is off, and gives administrators the line to run", async () => {
      setUrl(`/servers/view/?id=${ID}&tab=commands`);
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(off), ...install }));
      render(
        <As user={customerAdmin}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(screen.getByText("Off")).toBeInTheDocument();
      expect(screen.getByText(/Can only be turned on on the server/)).toBeInTheDocument();
      expect(screen.getByRole("link", { name: "how" })).toHaveAttribute("href", expect.stringContaining("tab=commands"));
      expect(screen.queryByRole("button", { name: "Turn on…" })).not.toBeInTheDocument();
      // First the line that lets this page turn them on (ADR-0098), then the one that turns them on for good.
      const [fromWeb, forGood] = await screen.findAllByLabelText("Command that allows AI commands");
      expect(fromWeb).toHaveTextContent("MF_AI_COMMANDS_FROM_WEB=1");
      expect(forGood).toHaveTextContent("MF_ALLOW_COMMANDS=1");
      expect(screen.getByText(/AI commands are off on web-1/)).toBeInTheDocument();
    });

    it("sends other people to an administrator", async () => {
      setUrl(`/servers/view/?id=${ID}&tab=commands`);
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(off) }));
      render(
        <As user={me({ role: "viewer", customer_id: null, site_id: null })}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(screen.getByText(/An administrator can turn them on/)).toBeInTheDocument();
      expect(screen.queryByLabelText("Command that allows AI commands")).not.toBeInTheDocument();
    });

    it("gives Windows administrators the PowerShell line", async () => {
      setUrl(`/servers/view/?id=${ID}&tab=commands`);
      const windows = { ...off, configure_sources: "unsupported", revert_sources: "unsupported" };
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(windows, { os_family: "windows" }) }));
      render(
        <As user={customerAdmin}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(screen.getByText(/AI commands are off on web-1/).parentElement).toHaveTextContent(
        "run this once on the server, in PowerShell as an administrator.",
      );
      const [fromWeb, line] = await screen.findAllByLabelText("Command that allows AI commands");
      expect(fromWeb).toHaveTextContent("ai-commands from-web on");
      expect(line).toHaveTextContent("ai-commands on");
      expect(line).not.toHaveTextContent("MF_ALLOW_COMMANDS");
    });

    it("sends an old Windows agent to its update, with nothing to turn on", async () => {
      setUrl(`/servers/view/?id=${ID}&tab=commands`);
      const windows = { ...off, shell: "unsupported", configure_sources: "unsupported", revert_sources: "unsupported" };
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(windows, { os_family: "windows" }) }));
      render(<ServerPage id={ID} />);
      await heading();
      expect(screen.getByText("Not available")).toBeInTheDocument();
      expect(screen.getByText(/Its agent is too old for AI commands on Windows/)).toBeInTheDocument();
      expect(screen.queryByRole("link", { name: "How to turn them on" })).not.toBeInTheDocument();
      expect(screen.queryByText(/AI commands are off/)).not.toBeInTheDocument();
    });

    it("lets administrators of the site turn them on where the server allowed it", async () => {
      const queued = command({ id: "c8", kind: "ai_commands", parameters: { on: true }, state: "queued" });
      const fetchMock = stubFetch(
        routes({
          [`GET /api/servers/${ID}`]: reporting({ ...off, ai_commands: "accepted" }),
          [`POST /api/servers/${ID}/ai-commands`]: { status: 201, body: { ...queued, stdout: null, stderr: null } },
        }),
      );
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      render(
        <As user={me({ role: "administrator", customer_id: null, site_id: web1.site_id })}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(screen.queryByText(/Can only be turned on on the server/)).not.toBeInTheDocument();
      await user.click(screen.getByRole("button", { name: "Turn on…" }));
      await user.click(screen.getByRole("button", { name: "Turn on" }));
      await vi.waitFor(() =>
        expect(fetchMock).toHaveBeenCalledWith(
          `/api/servers/${ID}/ai-commands`,
          expect.objectContaining({ method: "POST", body: JSON.stringify({ on: true }) }),
        ),
      );
    });

    it("offers no switch to operators", async () => {
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting({ ...off, ai_commands: "accepted" }) }));
      render(
        <As user={me({ role: "operator", customer_id: null, site_id: null })}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(screen.queryByRole("button", { name: "Turn on…" })).not.toBeInTheDocument();
    });

    it("lets administrators turn them off even where only the server can turn them on", async () => {
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting({ ...off, shell: "accepted", ai_commands: "off" }) }));
      render(
        <As user={customerAdmin}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(screen.getByRole("button", { name: "Turn off…" })).toBeInTheDocument();
    });

    it("names who turned them on, and what is under way", async () => {
      const change = {
        command_id: "c7",
        on: true,
        state: "succeeded",
        requested_by: "alice",
        issued_at: "2026-11-16T14:01:00Z",
        finished_at: "2026-11-16T14:02:00Z",
        error: null,
      } as const;
      const kinds = { ...off, shell: "accepted", ai_commands: "accepted" };
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(kinds, { ai_commands_change: change }) }));
      const { unmount } = render(
        <As user={customerAdmin}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(document.querySelector("dl")!).toHaveTextContent(/AI commands\s*On\s*Turned on by alice at 2026-11-16 \d\d:02\./);
      unmount();

      const pending = { ...change, on: false, state: "queued", finished_at: null } as const;
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(kinds, { ai_commands_change: pending, status: "offline" }) }));
      render(
        <As user={customerAdmin}>
          <ServerPage id={ID} />
        </As>,
      );
      await heading();
      expect(document.querySelector("dl")!).toHaveTextContent(/AI commands\s*On.*Turning off… asked by alice at .*\. It applies when the server is back\./);
      expect(screen.queryByRole("button", { name: "Turn off…" })).not.toBeInTheDocument();
    });

    it("dates what an offline server last said, and lists every kind under Details", async () => {
      stubFetch(routes({ [`GET /api/servers/${ID}`]: reporting(off, { status: "offline" }) }));
      render(<ServerPage id={ID} />);
      await heading();
      expect(screen.getByText(/As of its last contact/)).toBeInTheDocument();
      expect(screen.getByText("Accepts package updates, package sources, agent updates.")).toBeInTheDocument();
    });
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

  describe("sensor rules", () => {
    const operator = me({ role: "operator", site_id: "site-1" });
    const sent = (fetchMock: ReturnType<typeof stubFetch>, method: string, path: string) => {
      const call = fetchMock.mock.calls.find(([input, init]) => init?.method === method && String(input) === path);
      return call && JSON.parse(String(call[1]?.body));
    };
    function open(user: typeof operator, overrides: Record<string, unknown> = {}) {
      const fetchMock = stubFetch(routes(overrides));
      render(
        <As user={user}>
          <ServerPage id={ID} />
        </As>,
      );
      return fetchMock;
    }

    it("lands old Alerts and checks links on Sensors", async () => {
      setUrl(`/servers/view/?id=${ID}&tab=alerts`);
      open(operator);
      await heading();
      expect(screen.getByRole("tab", { name: "Sensors · 3" })).toHaveAttribute("aria-selected", "true");
      await vi.waitFor(() => expect(window.location.search).toBe(`?id=${ID}`));
      expect(screen.getAllByRole("tab").map((t) => t.textContent)).toEqual(["Sensors · 3", "Logs", "Updates", "Commands · 1 running"]);
      expect(screen.getByRole("button", { name: "Add a check" })).toBeInTheDocument();
      // How often built-in checks run is a site setting, under Settings, Sites.
      expect(screen.queryByText(/Built-in checks run/)).not.toBeInTheDocument();
    });

    it("opens a row's menu from the keyboard, Escape giving focus back", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      open(operator);
      const button = await screen.findByRole("button", { name: "Actions for CPU usage" });
      button.focus();
      await user.keyboard("{Enter}");
      const menu = screen.getByRole("menu", { name: "Actions for CPU usage" });
      const items = within(menu).getAllByRole("menuitem");
      expect(items.map((i) => i.textContent)).toEqual(["Edit", "Open chart", "Turn off…"]);
      expect(items[0]).toHaveFocus();
      expect(items[0]).toHaveAttribute("href", expect.stringMatching(/sensor=cpu_usage_ratio&instance=total&edit=1$/));
      await user.keyboard("{ArrowDown}");
      expect(items[1]).toHaveFocus();
      await user.keyboard("{ArrowUp}{ArrowUp}");
      expect(items[2]).toHaveFocus();
      await user.keyboard("{Escape}");
      expect(screen.queryByRole("menu")).not.toBeInTheDocument();
      expect(button).toHaveFocus();
    });

    it("offers readers the chart only", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      open(me({ role: "viewer", site_id: "site-1" }));
      await user.click(await screen.findByRole("button", { name: "Actions for CPU usage" }));
      expect(within(screen.getByRole("menu")).getAllByRole("menuitem").map((i) => i.textContent)).toEqual(["Open chart"]);
      expect(screen.queryByRole("button", { name: "Add a check" })).not.toBeInTheDocument();
    });

    it("turns off one disk's alert after confirming, and stops checking a whole sensor", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const fetchMock = open(operator, {
        [`GET /api/servers/${ID}/thresholds`]: ok({ entries: [] }),
        [`POST /api/servers/${ID}/thresholds`]: { status: 201, body: {} },
        [`POST /api/servers/${ID}/sensor_configs`]: { status: 201, body: {} },
      });
      await user.click(await screen.findByRole("button", { name: "Actions for Disk used /var" }));
      await user.click(screen.getByRole("menuitem", { name: "Turn off…" }));
      const confirm = screen.getByRole("form", { name: "Turn off alerts for Disk used /var on web-1?" });
      expect(confirm).toHaveTextContent("Its values are still recorded.");
      expect(within(confirm).getByRole("button", { name: "Turn off" })).toHaveFocus();
      await user.click(within(confirm).getByRole("button", { name: "Turn off" }));
      await vi.waitFor(() =>
        expect(sent(fetchMock, "POST", `/api/servers/${ID}/thresholds`)).toMatchObject({
          metric: "monitorflow_disk_used_ratio",
          instance: "/var",
          enabled: false,
        }),
      );
      await vi.waitFor(() => expect(screen.queryByRole("form", { name: /Turn off alerts/ })).not.toBeInTheDocument());

      await user.click(screen.getByRole("button", { name: "Actions for CPU usage" }));
      await user.click(screen.getByRole("menuitem", { name: "Turn off…" }));
      const stop = screen.getByRole("form", { name: "Stop checking CPU usage on this server?" });
      // Cancel leaves everything as it was.
      await user.click(within(stop).getByRole("button", { name: "Cancel" }));
      expect(sent(fetchMock, "POST", `/api/servers/${ID}/sensor_configs`)).toBeUndefined();
      await user.click(screen.getByRole("button", { name: "Actions for CPU usage" }));
      await user.click(screen.getByRole("menuitem", { name: "Turn off…" }));
      await user.click(within(screen.getByRole("form", { name: /Stop checking CPU usage/ })).getByRole("button", { name: "Turn off" }));
      await vi.waitFor(() =>
        expect(sent(fetchMock, "POST", `/api/servers/${ID}/sensor_configs`)).toMatchObject({ kind: "cpu", name: "cpu", enabled: false }),
      );
    });

    it("adds a check, which waits for its first value", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const port = { origin: "server" as const, entry_id: "p1", kind: "port", name: "port_443", instance: "", interval_ms: 60_000, timeout_ms: null, params: { port: "443", host: "127.0.0.1" } };
      const fetchMock = open(operator, {
        [`GET /api/servers/${ID}/sensors`]: [ok(config()), ok(config({ sensors: [...config().sensors, port] }))],
        "GET /api/plugins/usable": ok({ plugins: [] }),
        "GET /api/sites": ok({ sites: [] }),
        [`POST /api/servers/${ID}/sensor_configs`]: { status: 201, body: {} },
        [`GET /api/servers/${ID}/thresholds`]: ok({ entries: [] }),
        [`POST /api/servers/${ID}/thresholds`]: { status: 201, body: {} },
      });
      await user.click(await screen.findByRole("button", { name: "Add a check" }));
      const catalogue = await screen.findByRole("group", { name: "Check catalogue" });
      await user.click(within(catalogue).getAllByRole("button", { name: /^A port answers/ })[0]!);
      const form = screen.getByRole("form", { name: "Add a check" });
      await user.type(within(form).getByLabelText("Port"), "443");
      await user.click(within(form).getByRole("button", { name: "Add check" }));
      await vi.waitFor(() => expect(sent(fetchMock, "POST", `/api/servers/${ID}/sensor_configs`)).toMatchObject({ kind: "port", name: "port_443" }));
      const row = await screen.findByRole("row", { name: /Port port_443/ });
      expect(row).toHaveTextContent(/Waiting for first value/);
      expect(row).toHaveTextContent(/This server/);
    });

    it("lists the checks turned off on this server, to turn back on", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const offRow = { id: "o1", site_id: null, server_id: ID, kind: "memory", name: "memory", instance: "", interval_ms: 60_000, timeout_ms: null, params: {}, enabled: false };
      const fetchMock = open(operator, {
        [`GET /api/servers/${ID}/sensor_configs`]: ok({ entries: [offRow] }),
        "DELETE /api/sensor_configs/o1": { status: 204 },
      });
      const list = await screen.findByRole("region", { name: "Turned off on this server" });
      expect(list).toHaveTextContent("Memory");
      await user.click(within(list).getByRole("button", { name: "Turn back on" }));
      await vi.waitFor(() =>
        expect(fetchMock).toHaveBeenCalledWith("/api/sensor_configs/o1", expect.objectContaining({ method: "DELETE" })),
      );
    });
  });

  it("lets administrators of the customer remove the server, then shows how to uninstall it", async () => {
    // Other customers' administrators and viewers get no button.
    stubFetch(routes());
    const { unmount } = render(
      <As user={me({ role: "administrator", customer_id: "c2", site_id: null })}>
        <ServerPage id={ID} />
      </As>,
    );
    await heading();
    expect(screen.queryByRole("button", { name: /Remove/ })).not.toBeInTheDocument();
    unmount();

    const fetchMock = stubFetch(
      routes({
        [`GET /api/servers/${ID}`]: [ok(web1), ok({ ...web1, revoked_at: "2026-10-22T10:00:00Z", status: "offline" })],
        [`POST /api/servers/${ID}/revoke`]: ok({
          revoked_at: "2026-10-22T10:00:00Z",
          reason: "cessation_of_operation",
          serials: ["3f2a"],
          closed_streams: 1,
        }),
        "GET /api/settings/agent-install": ok({
          source: "github",
          custom_url: null,
          source_from_config: false,
          custom_url_from_config: false,
          github_url: "https://github.com/x/releases/latest/download",
          central: null,
        }),
        "GET /api/settings/public-address": ok({ address: "mf.example" }),
      }),
    );
    render(
      <As user={customerAdmin}>
        <ServerPage id={ID} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "More actions for web-1" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Remove web-1…" }));
    expect(screen.getByRole("form", { name: "Remove web-1?" })).toHaveTextContent("history is kept");
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/revoke"))).toBe(false);
    await userEvent.click(screen.getByRole("button", { name: "Remove web-1" }));
    const revoke = fetchMock.mock.calls.find(([url]) => String(url).endsWith("/revoke"));
    expect(JSON.parse(String(revoke?.[1]?.body))).toEqual({ reason: "cessation_of_operation" });
    expect(await screen.findByText("Removed", { selector: "span" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Remove/ })).not.toBeInTheDocument();
    const panel = await screen.findByRole("region", { name: "Removed" });
    expect(await within(panel).findByText(/MF_UNINSTALL=1 MF_PURGE=1/)).toHaveTextContent(
      "curl -fsSL https://github.com/x/releases/latest/download/install.sh",
    );
    expect(within(panel).getByRole("link", { name: "Add servers" })).toHaveAttribute("href", expect.stringContaining("/settings/add-servers"));
  });

  it("shows readers of a removed server no uninstall command", async () => {
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok({ ...web1, revoked_at: "2026-10-22T10:00:00Z", status: "offline" }) }));
    render(<ServerPage id={ID} />);
    const panel = await screen.findByRole("region", { name: "Removed" });
    expect(panel).toHaveTextContent("web-1 was removed");
    expect(within(panel).queryByRole("link")).not.toBeInTheDocument();
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

  it("warns about late renewal or a revoked certificate, not a healthy one", async () => {
    const certificate = { ...web1.certificate!, not_after: inDays(1.4) };
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok({ ...web1, certificate }) }));
    const { unmount } = render(<ServerPage id={ID} />);
    await heading();
    expect(within(header()).getByRole("tooltip", { hidden: true })).toHaveTextContent("Renewal late, expires in 1 day");
    expect(within(document.querySelector("dl")!).getByText("Renewal late, expires in 1 day")).toBeInTheDocument();
    unmount();

    const revoked = { ...certificate, revoked: true, revoked_at: "2026-10-19T08:00:00", revocation_reason: "key_compromise" };
    stubFetch(routes({ [`GET /api/servers/${ID}`]: ok({ ...web1, certificate: revoked }) }));
    render(<ServerPage id={ID} />);
    await heading();
    expect(within(document.querySelector("dl")!).getByText("Revoked").closest("dd")).toHaveTextContent("Revoked 2026-10-19 08:00 (key compromise)");
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
    expect(document.querySelector("dl")).toHaveTextContent(/Agent\s*0\.3\.0 available · via relay-paris-1/);
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
    expect(document.querySelector("dl")).toHaveTextContent(/Agent\s*via relay-paris-1/);
  });
});
