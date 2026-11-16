import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { EffectiveSensors, ServerDetail } from "@/lib/api";
import { As, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { SensorPage } from "./SensorPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
const NOW = new Date("2026-10-22T14:32:05");
const t = (h: number, m: number) => new Date(2026, 9, 22, h, m).getTime() / 1000;

const server = { id: ID, hostname: "web-1", site_name: "paris", customer_name: "acme" } as ServerDetail;
const config: EffectiveSensors = {
  server_id: ID,
  site_id: "site-1",
  check_interval_ms: 60_000,
  check_interval_origin: "default",
  revision: 7,
  applied_revision: 6,
  applied_at: null,
  in_sync: false,
  sensors: [
    { origin: "server", entry_id: "e1", kind: "disk", name: "disk", instance: "", interval_ms: 30_000, timeout_ms: 10_000, params: {} },
  ],
};
const thresholds = {
  server_id: ID,
  site_id: "site-1",
  revision: 3,
  thresholds: [
    { origin: "site", threshold_id: "t1", metric: "monitorflow_disk_used_ratio", instance: null, enabled: true, comparison: ">", warning: 0.8, critical: 0.9, duration_s: 300, hysteresis: 0.02 },
  ],
};
const open = incident({ server_id: ID, opened_at: new Date(2026, 9, 22, 11, 40).toISOString(), peak_value: 0.946, reason: "94.2% used is above the critical limit of 90%" });
const old = incident({
  id: "2d6a9a3e-5b7c-4d8e-9f01-23456789abcd",
  server_id: ID,
  severity: "warning",
  state: "resolved",
  opened_at: new Date(2026, 9, 14, 3, 10).toISOString(),
  resolved_at: new Date(2026, 9, 14, 3, 58).toISOString(),
  peak_value: 0.839,
});
const other = incident({ id: "3d6a9a3e-5b7c-4d8e-9f01-23456789abcd", server_id: ID, instance: "/" });

function stub(overrides: Record<string, unknown> = {}, range: { status: number; body: unknown } = { status: 200, body: { query: "", series: [{ labels: { instance: "/var" }, points: [[t(13, 0), 0.9], [t(14, 30), 0.942]] }] } }) {
  const fetchMock = stubFetch({
    [`GET /api/servers/${ID}`]: { status: 200, body: server },
    [`GET /api/servers/${ID}/sensors`]: { status: 200, body: config },
    [`GET /api/servers/${ID}/thresholds/effective`]: { status: 200, body: thresholds },
    [`GET /api/incidents?server_id=${ID}&limit=500`]: { status: 200, body: { incidents: [open, other, old], next_cursor: null } },
    ...overrides,
  } as Parameters<typeof stubFetch>[0]);
  fetchMock.mockImplementation(
    ((original) => async (input, init) => {
      if (String(input).startsWith("/api/metrics/query_range")) {
        return new Response(JSON.stringify(range.body), { status: range.status });
      }
      return original(input, init);
    })(fetchMock.getMockImplementation()!),
  );
  return fetchMock;
}

const rangeCalls = (fetchMock: ReturnType<typeof stubFetch>) =>
  fetchMock.mock.calls
    .map(([input]) => new URL(String(input), "http://localhost"))
    .filter((url) => url.pathname === "/api/metrics/query_range")
    .map((url) => Object.fromEntries(url.searchParams));

describe("SensorPage", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl(`/servers/sensor/?id=${ID}&sensor=disk_used_ratio&instance=%2Fvar`);
  });
  afterEach(() => vi.useRealTimers());

  it("shows the counters, the history with thresholds, the configuration and the incidents", async () => {
    const fetchMock = stub();
    render(<SensorPage id={ID} sensor="disk_used_ratio" instance="/var" />);
    expect(await screen.findByRole("heading", { level: 1, name: "Disk used /var" })).toBeInTheDocument();
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent(/^Fleet›web-1▾›▾$/);
    expect(screen.getByText(/on web-1 · disk sensor · every 30 s/)).toBeInTheDocument();
    expect(screen.getByText("94.2% used is above the critical limit of 90%")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open incident" })).toHaveAttribute("href", expect.stringContaining(open.id));

    expect(screen.getByRole("group", { name: "Summary" })).toHaveTextContent(
      /Current94\.2%\s*Peak, 6 h94\.2%\s*Average, 6 h92\.1%\s*Above critical for2 h 52 min/,
    );
    const chart = screen.getByRole("figure", { name: "History" });
    expect(within(chart).getAllByTestId("chart-threshold")).toHaveLength(2);
    expect(chart).toHaveTextContent(/Warning 80%.*Critical 90%/);
    expect(within(chart).getByTestId("chart-marker")).toHaveTextContent("Opened 11:40");
    expect(within(chart).getByTestId("chart-band")).toBeInTheDocument();
    expect(within(chart).getByRole("button", { name: "Download CSV" })).toBeInTheDocument();
    expect(rangeCalls(fetchMock)[0]).toMatchObject({ metric: "monitorflow_disk_used_ratio", instance: "/var", step: "120" });

    const configuration = screen.getByRole("region", { name: "Configuration" });
    expect(configuration).toHaveTextContent(/Kind\s*disk\s*Watches\s*\/var\s*How often\s*every 30 s\s*Timeout\s*10 s/);
    expect(configuration).toHaveTextContent(/Warning\s*> 80%\s*Critical\s*> 90%\s*Duration\s*5 min/);
    expect(configuration).toHaveTextContent(/Recovery margin\s*2% below the threshold before resolving/);
    expect(configuration).toHaveTextContent(/Applies from\s*This server/);
    expect(configuration).toHaveTextContent(/Set by\s*How often: This server · thresholds: Site default/);
    // Viewers see the configuration, not the way to change it.
    expect(within(configuration).queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(configuration).toHaveTextContent(/Configuration\s*Out of date/);

    const incidents = screen.getByRole("region", { name: "Incidents on this sensor" });
    expect(incidents).toHaveTextContent("1 open · 2 in 30 days");
    const rows = within(incidents).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent)).toEqual([
      "Critical open11:40 today2 h 52 min94.6%",
      "Resolved was Warning14 Oct 03:1048 min83.9%",
    ]);
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stub();
    render(<SensorPage id={ID} sensor="disk_used_ratio" instance="/var" />);
    expect(await screen.findByRole("heading", { level: 1, name: "Disque utilisé /var" })).toBeInTheDocument();
    expect(screen.getByText(/sur web-1 · capteur disk · toutes les 30 s/)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Résumé" })).toHaveTextContent(/Actuel94,2 %.*Au-delà du seuil critique depuis2 h 52 min/);
    const configuration = screen.getByRole("region", { name: "Configuration" });
    expect(configuration).toHaveTextContent(/Fréquence\s*toutes les 30 s\s*Délai\s*10 s/);
    expect(configuration).toHaveTextContent(/S’applique depuis\s*Ce serveur/);
    const incidents = screen.getByRole("region", { name: "Incidents de ce capteur" });
    expect(incidents).toHaveTextContent("ouverts : 1 · sur 30 jours : 2");
  });

  it("changes the range through the URL", async () => {
    const fetchMock = stub();
    render(<SensorPage id={ID} sensor="disk_used_ratio" instance="/var" />);
    await screen.findByRole("figure", { name: "History" });
    await userEvent.click(screen.getByRole("button", { name: "7 d" }));
    expect(window.location.search).toContain("range=7d");
    await vi.waitFor(() => expect(rangeCalls(fetchMock).at(-1)).toMatchObject({ step: "1800", rollup: "max" }));
    expect(screen.getByRole("button", { name: "7 d" })).toHaveAttribute("aria-pressed", "true");
  });

  it("explains a metrics backend failure and a sensor without thresholds", async () => {
    stub(
      {
        [`GET /api/servers/${ID}/thresholds/effective`]: { status: 200, body: { ...thresholds, thresholds: [] } },
        [`GET /api/incidents?server_id=${ID}&limit=500`]: { status: 200, body: { incidents: [], next_cursor: null } },
      },
      { status: 502, body: { error: "bad gateway" } },
    );
    render(<SensorPage id={ID} sensor="disk_used_ratio" instance="/var" />);
    expect(await screen.findByRole("alert")).toHaveTextContent("The metrics backend is unavailable");
    expect(screen.getByText("none: this sensor never opens incidents")).toBeInTheDocument();
    expect(screen.getByText("No incidents on this sensor.")).toBeInTheDocument();
  });

  it("charts a disk's activity under its busy share", async () => {
    const diskio = { origin: "default", entry_id: null, kind: "diskio", name: "diskio", instance: "", interval_ms: 60_000, timeout_ms: null, params: {} };
    const fetchMock = stub(
      {
        [`GET /api/servers/${ID}/sensors`]: { status: 200, body: { ...config, sensors: [diskio] } },
        [`GET /api/incidents?server_id=${ID}&limit=500`]: { status: 200, body: { incidents: [], next_cursor: null } },
      },
      { status: 200, body: { query: "", series: [{ labels: { instance: "sda" }, points: [[t(13, 0), 2048], [t(14, 30), 4096]] }] } },
    );
    render(<SensorPage id={ID} sensor="diskio_busy_ratio" instance="sda" />);
    expect(await screen.findByRole("heading", { level: 1, name: "Disk busy sda" })).toBeInTheDocument();
    const read = screen.getByRole("figure", { name: "Read and written" });
    expect(read).toHaveTextContent("Read");
    expect(read).toHaveTextContent("Written");
    for (const name of ["Reads and writes", "Average wait per I/O", "I/Os in flight"]) {
      expect(screen.getByRole("figure", { name })).toBeInTheDocument();
    }
    expect(screen.queryByRole("figure", { name: "Busy" })).not.toBeInTheDocument();
    const metrics = rangeCalls(fetchMock).map((c) => c.metric);
    expect(new Set(metrics)).toEqual(
      new Set(
        ["busy_ratio", "read_bytes_per_second", "written_bytes_per_second", "reads_per_second", "writes_per_second", "wait_seconds", "queue_depth"].map(
          (m) => `monitorflow_diskio_${m}`,
        ),
      ),
    );
    expect(rangeCalls(fetchMock).every((c) => c.instance === "sda")).toBe(true);
  });

  it("says in the header when the sensor's data is late", async () => {
    stub({
      [`GET /api/incidents?server_id=${ID}&limit=500`]: { status: 200, body: { incidents: [], next_cursor: null } },
      [`GET /api/servers/${ID}/sensor-values`]: {
        status: 200,
        body: {
          start: 0,
          end: 3600,
          step: 120,
          sensors: [
            {
              name: config.sensors[0]!.name,
              instance: config.sensors[0]!.instance,
              metric: "monitorflow_disk_used_ratio",
              series: [{ instance: "/var", points: [], last_value: 0.94, last_at: null, reason: "No data for 15 minutes" }],
              reason: null,
            },
          ],
        },
      },
    });
    render(<SensorPage id={ID} sensor="disk_used_ratio" instance="/var" />);
    expect(await screen.findByText("No data for 15 minutes")).toBeInTheDocument();
    expect(screen.getAllByText("Unknown").length).toBeGreaterThan(0);
  });

  describe("editing the configuration", () => {
    const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });
    const editable = { ...server, site_id: "site-1", customer_id: "c1", revoked_at: null };
    const hourly = { ...config.sensors[0]!, origin: "site" as const, entry_id: "s1", interval_ms: 3_600_000, timeout_ms: null };

    function edit(overrides: Record<string, unknown> = {}, page = { sensor: "disk_used_ratio", instance: "/var" }) {
      const fetchMock = stub({
        [`GET /api/servers/${ID}`]: { status: 200, body: editable },
        [`GET /api/servers/${ID}/sensors`]: { status: 200, body: { ...config, sensors: [hourly] } },
        [`GET /api/servers/${ID}/sensor_configs`]: { status: 200, body: { entries: [] } },
        "GET /api/sites/site-1/sensors": { status: 200, body: { site_id: "site-1", check_interval_ms: 60_000, check_interval_origin: "default", sensors: [hourly] } },
        [`POST /api/servers/${ID}/sensor_configs`]: { status: 201, body: {} },
        [`GET /api/servers/${ID}/thresholds`]: { status: 200, body: { entries: [] } },
        [`POST /api/servers/${ID}/thresholds`]: { status: 201, body: {} },
        "GET /api/sites/site-1/thresholds": { status: 200, body: { entries: [{ id: "t1", metric: "monitorflow_disk_used_ratio", instance: null }] } },
        "PUT /api/thresholds/t1": { status: 200, body: {} },
        ...overrides,
      });
      render(
        <As user={operator}>
          <SensorPage id={ID} sensor={page.sensor} instance={page.instance} />
        </As>,
      );
      return fetchMock;
    }

    const sent = (fetchMock: ReturnType<typeof stubFetch>, method: string, path: string) => {
      const call = fetchMock.mock.calls.find(([input, init]) => init?.method === method && String(input) === path);
      return call && JSON.parse(String(call[1]?.body));
    };

    async function openEditor(user: ReturnType<typeof userEvent.setup>) {
      const configuration = await screen.findByRole("region", { name: "Configuration" });
      await user.click(await within(configuration).findByRole("button", { name: "Edit" }));
      return within(configuration).getByRole("form", { name: "Edit the configuration" });
    }

    it("says what a slow check does to the alert", async () => {
      const fetchMock = edit();
      const configuration = await screen.findByRole("region", { name: "Configuration" });
      await within(configuration).findByRole("button", { name: "Edit" });
      expect(configuration).toHaveTextContent(/Timeout\s*automatic, 1 min/);
      expect(configuration).toHaveTextContent("Checked every 1 h, so this alert opens up to 2 h after the problem starts.");
      expect(screen.getByText(/· checked every 1 h$/)).toBeInTheDocument();
      // One point an hour, each step summarizing the hour.
      expect(rangeCalls(fetchMock)[0]).toMatchObject({ step: "3600", rollup: "max" });
    });

    it("counts the servers each choice reaches, and confirms before turning the alert off", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const servers = [editable, { ...editable, id: "s2" }, { ...editable, id: "s3", revoked_at: "2026-10-01T00:00:00Z" }];
      const fetchMock = edit({ "GET /api/servers?site_id=site-1&limit=500": { status: 200, body: { servers, next_cursor: null } } });
      const form = await openEditor(user);
      expect(await within(form).findByText("· 2 servers")).toBeInTheDocument();
      expect(within(form).getByRole("radio", { name: /^every server of paris \(site default\)/ }).closest("label")).toHaveTextContent(
        "every server of paris (site default) · 2 servers",
      );
      expect(within(form).getByPlaceholderText("No warning")).toBeInTheDocument();
      expect(within(form).queryByText("Empty: no warning.")).not.toBeInTheDocument();
      const writes = () => fetchMock.mock.calls.filter(([, init]) => init?.method && init.method !== "GET").length;
      await user.click(within(form).getByRole("button", { name: "Turn this alert off…" }));
      expect(writes()).toBe(0);
      expect(within(form).getByText("Turn this alert off?")).toBeInTheDocument();
      await user.click(within(form).getByRole("button", { name: "Turn off" }));
      await vi.waitFor(() => expect(writes()).toBe(1));
    });

    it("preselects the level in effect and saves the interval and timeout for this server", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const fetchMock = edit();
      const form = await openEditor(user);
      expect(within(form).getByRole("radio", { name: "every server of paris (site default)" })).toBeChecked();
      expect(within(form).getAllByRole("radio").map((r) => r.closest("label")?.textContent?.trim())).toEqual([
        "this sensor only (/var) · 1 server",
        "every disk on web-1 · 1 server",
        "every server of paris (site default)",
      ]);
      await user.selectOptions(within(form).getByRole("combobox", { name: "How often to check" }), "every 5 min");
      await user.type(within(form).getByLabelText("Timeout"), "20");
      await user.click(within(form).getByRole("radio", { name: /^every disk on web-1/ }));
      await user.click(within(form).getByRole("button", { name: "Save" }));
      await vi.waitFor(() =>
        expect(sent(fetchMock, "POST", `/api/servers/${ID}/sensor_configs`)).toEqual({
          kind: "disk",
          name: "disk",
          instance: "",
          params: {},
          enabled: true,
          interval_ms: 300_000,
          timeout_ms: 20_000,
        }),
      );
      // Only what changed is written: the thresholds keep following the site.
      expect(sent(fetchMock, "POST", `/api/servers/${ID}/thresholds`)).toBeUndefined();
    });

    it("saves thresholds for this sensor only, or for the whole site", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const fetchMock = edit();
      let form = await openEditor(user);
      await user.click(within(form).getByRole("radio", { name: /^this sensor only \(\/var\)/ }));
      await user.clear(within(form).getByLabelText("Warning at"));
      await user.type(within(form).getByLabelText("Warning at"), "70");
      await user.click(within(form).getByRole("button", { name: "Save" }));
      await vi.waitFor(() =>
        expect(sent(fetchMock, "POST", `/api/servers/${ID}/thresholds`)).toEqual({
          metric: "monitorflow_disk_used_ratio",
          instance: "/var",
          comparison: ">",
          warning: 0.7,
          critical: 0.9,
          duration_s: 300,
          hysteresis: 0.02,
          enabled: true,
        }),
      );
      // Nothing else changed: the timing stays inherited.
      expect(sent(fetchMock, "POST", `/api/servers/${ID}/sensor_configs`)).toBeUndefined();

      form = await openEditor(user);
      await user.clear(within(form).getByLabelText("Critical at"));
      await user.type(within(form).getByLabelText("Critical at"), "95");
      await user.click(within(form).getByRole("button", { name: "Save" }));
      await vi.waitFor(() => expect(sent(fetchMock, "PUT", "/api/thresholds/t1")).toMatchObject({ instance: null, critical: 0.95 }));
    });

    it("refuses a timeout longer than the interval and a critical level under the warning", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const fetchMock = edit({ [`GET /api/servers/${ID}/sensors`]: { status: 200, body: config } });
      const form = await openEditor(user);
      await user.clear(within(form).getByLabelText("Timeout"));
      await user.type(within(form).getByLabelText("Timeout"), "45");
      await user.clear(within(form).getByLabelText("Critical at"));
      await user.type(within(form).getByLabelText("Critical at"), "50");
      await user.click(within(form).getByRole("button", { name: "Save" }));
      expect(within(form).getByText("Enter a number of seconds up to 30 s, or leave it empty.")).toBeInTheDocument();
      expect(within(form).getByText("The critical level must be at or above the warning level.")).toBeInTheDocument();
      expect(fetchMock.mock.calls.some(([, init]) => init?.method && init.method !== "GET")).toBe(false);
    });

    it("resets this sensor's own threshold to the inherited one", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const own = { ...thresholds.thresholds[0]!, origin: "server", threshold_id: "t2", instance: "/var", warning: 0.6 };
      const fetchMock = edit({
        [`GET /api/servers/${ID}/thresholds/effective`]: { status: 200, body: { ...thresholds, thresholds: [own] } },
        "DELETE /api/thresholds/t2": { status: 204 },
      });
      const configuration = await screen.findByRole("region", { name: "Configuration" });
      await within(configuration).findByRole("button", { name: "Edit" });
      expect(configuration).toHaveTextContent(/Applies from\s*This sensor/);
      await user.click(within(configuration).getByRole("button", { name: "Reset to inherited" }));
      await vi.waitFor(() =>
        expect(fetchMock).toHaveBeenCalledWith("/api/thresholds/t2", expect.objectContaining({ method: "DELETE" })),
      );
      // The editor preselects the level in effect.
      const form = await openEditor(user);
      expect(within(form).getByRole("radio", { name: /^this sensor only \(\/var\)/ })).toBeChecked();
    });

    it("opens the form from the list's Edit, and drops it from the address once closed", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      setUrl(`/servers/sensor/?id=${ID}&sensor=disk_used_ratio&instance=%2Fvar&edit=1`);
      edit();
      const form = await screen.findByRole("form", { name: "Edit the configuration" });
      await user.click(within(form).getByRole("button", { name: "Cancel" }));
      expect(screen.queryByRole("form", { name: "Edit the configuration" })).not.toBeInTheDocument();
      expect(window.location.search).not.toContain("edit=");
    });

    it("words a pass or fail check as when it fails, without levels", async () => {
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime.bind(vi) });
      const port = { origin: "server", entry_id: "p1", kind: "port", name: "port_443", instance: "", interval_ms: 60_000, timeout_ms: null, params: { port: "443" } };
      const rule = { origin: "server", threshold_id: "t3", metric: "monitorflow_port_443_up", instance: null, enabled: true, comparison: "<", warning: null, critical: 1, duration_s: 60, hysteresis: 0 };
      setUrl(`/servers/sensor/?id=${ID}&sensor=port_443_up`);
      edit(
        {
          [`GET /api/servers/${ID}/sensors`]: { status: 200, body: { ...config, sensors: [port] } },
          [`GET /api/servers/${ID}/thresholds/effective`]: { status: 200, body: { ...thresholds, thresholds: [rule] } },
          "GET /api/sites/site-1/sensors": { status: 200, body: { site_id: "site-1", check_interval_ms: 60_000, check_interval_origin: "default", sensors: [] } },
        },
        { sensor: "port_443_up", instance: "" },
      );
      const configuration = await screen.findByRole("region", { name: "Configuration" });
      await within(configuration).findByRole("button", { name: "Edit" });
      expect(configuration).toHaveTextContent(/Alert\s*Critical when it fails\s*Duration\s*1 min/);
      // A check added on this server has nothing to go back to.
      expect(within(configuration).queryByRole("button", { name: "Reset to inherited" })).not.toBeInTheDocument();
      const form = await openEditor(user);
      expect(within(form).getByRole("combobox", { name: "Alert" })).toHaveDisplayValue("Critical when it fails");
      expect(within(form).queryByLabelText("Warning at")).not.toBeInTheDocument();
      expect(within(form).getAllByRole("radio")).toHaveLength(2);
    });
  });

  it("does not call the API for a malformed sensor", () => {
    const fetchMock = stubFetch({});
    render(<SensorPage id={ID} sensor="../metrics" instance="" />);
    expect(screen.getByText("Sensor not found")).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("says when the server is not found", async () => {
    stub({ [`GET /api/servers/${ID}`]: { status: 404, body: { error: "not found" } } });
    render(<SensorPage id={ID} sensor="disk_used_ratio" instance="/var" />);
    expect(await screen.findByText("Sensor not found")).toBeInTheDocument();
  });
});
