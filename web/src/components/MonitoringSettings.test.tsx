import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { EffectiveSensor, EffectiveThreshold } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { MonitoringSettings, type Target } from "./MonitoringSettings";

vi.mock("next/navigation", () => import("@/test/navigation"));

const SERVER: Target = { kind: "server", id: "srv-1", siteId: "site-1", siteName: "paris", osFamily: "debian" };
const SITE: Target = { kind: "site", id: "site-1", siteName: "paris" };
const DISK = "monitorflow_disk_used_ratio";

const disk = (overrides: Partial<EffectiveThreshold> = {}): EffectiveThreshold => ({
  origin: "default",
  threshold_id: null,
  metric: DISK,
  instance: null,
  enabled: true,
  comparison: ">",
  warning: 0.85,
  critical: 0.95,
  duration_s: 300,
  hysteresis: 0.02,
  ...overrides,
});

const sensor = (overrides: Partial<EffectiveSensor>): EffectiveSensor => ({
  origin: "default",
  entry_id: null,
  kind: "disk",
  name: "disk",
  instance: "",
  interval_ms: 60_000,
  timeout_ms: null,
  params: {},
  ...overrides,
});

const port = sensor({ origin: "server", entry_id: "p1", kind: "port", name: "port_443", params: { port: "443", host: "127.0.0.1" } });

const INTERVAL = { check_interval_ms: 60_000, check_interval_origin: "default" };

/** The GET routes of the server panel; `site` is what the site runs. */
function serverRoutes(thresholds: EffectiveThreshold[], sensors: EffectiveSensor[], rows: unknown[] = [], site: EffectiveSensor[] = sensors) {
  return {
    "GET /api/servers/srv-1/thresholds/effective": { status: 200, body: { server_id: "srv-1", site_id: "site-1", revision: 1, thresholds } },
    "GET /api/servers/srv-1/sensors": {
      status: 200,
      body: { server_id: "srv-1", site_id: "site-1", ...INTERVAL, revision: 1, applied_revision: 1, applied_at: null, in_sync: true, sensors },
    },
    "GET /api/servers/srv-1/sensor_configs": { status: 200, body: { entries: rows } },
    "GET /api/sites/site-1/sensors": { status: 200, body: { site_id: "site-1", ...INTERVAL, sensors: site } },
  };
}

const bodyOf = (fetchMock: ReturnType<typeof stubFetch>, key: string) => {
  const call = fetchMock.mock.calls.find(([input, init]) => `${init?.method ?? "GET"} ${String(input)}` === key);
  return JSON.parse(String(call?.[1]?.body)) as unknown;
};

describe("MonitoringSettings", () => {
  it("shows alerts in plain words with where they come from, read-only without a role", async () => {
    stubFetch(serverRoutes([disk(), disk({ instance: "/data", origin: "site", threshold_id: "t1", warning: 0.7 })], [sensor({}), port]));
    render(<MonitoringSettings target={SERVER} canEdit={false} />);

    const alerts = await screen.findByRole("region", { name: "When to alert" });
    const first = within(alerts).getByText("Warn when a disk is over 85% full, critical over 95%, for 5 min.").closest("li")!;
    expect(first).toHaveTextContent("Built-in");
    const site = within(alerts).getByText(/disk \/data is over 70% full/).closest("li")!;
    expect(site).toHaveTextContent("Site default");
    const checks = screen.getByRole("region", { name: "What is checked" });
    expect(within(checks).getByText("Port 443").closest("li")).toHaveTextContent("This server only");
    expect(screen.getByText(/Changing them needs an operator role/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add a check" })).not.toBeInTheDocument();
  });

  it("edits a built-in alert for this server with inline validation", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...serverRoutes([disk()], [sensor({})]),
      "GET /api/servers/srv-1/thresholds": { status: 200, body: { entries: [] } },
      "POST /api/servers/srv-1/thresholds": { status: 201, body: {} },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const form = screen.getByRole("form", { name: "Edit alert" });
    const warning = within(form).getByLabelText("Warning at");
    expect(warning).toHaveValue("85");
    await user.clear(warning);
    await user.type(warning, "99");
    await user.click(within(form).getByRole("button", { name: "Save" }));
    expect(within(form).getByRole("alert")).toHaveTextContent("The critical level must be at or above the warning level.");
    expect(within(form).getByLabelText("Critical at")).toHaveAttribute("aria-invalid", "true");

    await user.clear(warning);
    await user.type(warning, "70");
    await user.click(within(form).getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("button", { name: "Edit" })).toBeInTheDocument();
    expect(bodyOf(fetchMock, "POST /api/servers/srv-1/thresholds")).toEqual({
      metric: DISK,
      instance: null,
      comparison: ">",
      warning: 0.7,
      critical: 0.95,
      duration_s: 300,
      hysteresis: 0.02,
      enabled: true,
    });
  });

  it("saves for the whole site by replacing the site's row", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...serverRoutes([disk({ origin: "server", threshold_id: "own" })], [sensor({})]),
      "GET /api/sites/site-1/thresholds": { status: 200, body: { entries: [{ id: "row-1", metric: DISK, instance: null }] } },
      "PUT /api/thresholds/row-1": { status: 200, body: {} },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("radio", { name: /Every server of paris/ }));
    expect(screen.getByText("(this server keeps its own value)")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("button", { name: "Edit" })).toBeInTheDocument();
    expect(bodyOf(fetchMock, "PUT /api/thresholds/row-1")).toMatchObject({ warning: 0.85, critical: 0.95 });
  });

  it("goes back to the site default by deleting the server's row", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...serverRoutes([disk({ origin: "server", threshold_id: "own" })], [sensor({})]),
      "DELETE /api/thresholds/own": { status: 204 },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Use the site default" }));
    expect(fetchMock.mock.calls.some(([input, init]) => init?.method === "DELETE" && String(input) === "/api/thresholds/own")).toBe(true);
  });

  it("adds a check with its alert, on the site", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/sites/site-1/thresholds/effective": { status: 200, body: { site_id: "site-1", thresholds: [disk()] } },
      "GET /api/sites/site-1/sensors": {
        status: 200,
        body: { site_id: "site-1", check_interval_ms: 300_000, check_interval_origin: "site", sensors: [sensor({ interval_ms: 300_000 })] },
      },
      "GET /api/sites/site-1/sensor_configs": { status: 200, body: { entries: [] } },
      "POST /api/sites/site-1/sensor_configs": [{ status: 409, body: { error: "exists" } }, { status: 201, body: {} }],
      "GET /api/sites/site-1/thresholds": { status: 200, body: { entries: [] } },
      "POST /api/sites/site-1/thresholds": { status: 201, body: {} },
    });
    render(<MonitoringSettings target={SITE} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Add a check" }));
    const form = screen.getByRole("form", { name: "Add a check" });
    // A site can mix systems, so every kind is offered.
    expect(within(within(form).getByLabelText("Check that")).getAllByRole("option")).toHaveLength(6);
    // It starts at the site's interval; "Other" takes any value.
    const often = within(form).getByRole("combobox", { name: "How often to check" });
    expect(often).toHaveDisplayValue("every 5 min");
    await user.selectOptions(often, "Other…");
    await user.clear(within(form).getByLabelText("Every"));
    await user.type(within(form).getByLabelText("Every"), "2");
    await user.selectOptions(within(form).getByLabelText("Unit"), "seconds");
    await user.click(within(form).getByRole("button", { name: "Set" }));
    expect(within(form).getByRole("alert")).toHaveTextContent("Enter a value from 10 seconds to 24 hours.");
    await user.selectOptions(within(form).getByLabelText("Unit"), "minutes");
    await user.click(within(form).getByRole("button", { name: "Set" }));
    expect(often).toHaveDisplayValue("every 2 min");
    expect(within(form).getByLabelText("Address")).toHaveValue("127.0.0.1");
    await user.click(within(form).getByRole("button", { name: "Add check" }));
    expect(within(form).getByRole("alert")).toHaveTextContent("Enter the port.");

    await user.type(within(form).getByLabelText("Port"), "5432");
    await user.click(within(form).getByRole("button", { name: "Add check" }));
    expect(await within(form).findByText("This check already exists here.")).toBeInTheDocument();
    await user.click(within(form).getByRole("button", { name: "Add check" }));
    expect(await screen.findByRole("button", { name: "Add a check" })).toBeInTheDocument();
    expect(bodyOf(fetchMock, "POST /api/sites/site-1/sensor_configs")).toEqual({
      kind: "port",
      name: "port_5432",
      interval_ms: 120_000,
      params: { port: "5432", host: "127.0.0.1" },
    });
    expect(bodyOf(fetchMock, "POST /api/sites/site-1/thresholds")).toMatchObject({ metric: "monitorflow_port_5432_up", critical: 1 });
  });

  it("removes a check added here with its alerts, and turns off an inherited one", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...serverRoutes([disk()], [sensor({}), port]),
      "GET /api/servers/srv-1/thresholds": {
        status: 200,
        body: { entries: [{ id: "t-port", metric: "monitorflow_port_443_up" }, { id: "t-disk", metric: DISK }] },
      },
      "DELETE /api/thresholds/t-port": { status: 204 },
      "DELETE /api/sensor_configs/p1": { status: 204 },
      "POST /api/servers/srv-1/sensor_configs": { status: 201, body: {} },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Remove Port 443" }));
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await screen.findByRole("button", { name: "Remove Port 443" });
    const deleted = fetchMock.mock.calls.filter(([, init]) => init?.method === "DELETE").map(([input]) => String(input));
    expect(deleted).toEqual(["/api/thresholds/t-port", "/api/sensor_configs/p1"]);

    await user.click(screen.getByRole("button", { name: "Turn off Disk space" }));
    await user.click(screen.getByRole("button", { name: "Turn off" }));
    await screen.findByRole("button", { name: "Turn off Disk space" });
    expect(bodyOf(fetchMock, "POST /api/servers/srv-1/sensor_configs")).toMatchObject({ kind: "disk", name: "disk", enabled: false });
  });

  it("sets how often the built-in checks of the site run", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...serverRoutes([disk()], [sensor({})]),
      "PUT /api/sites/site-1/check-interval": [{ status: 200, body: { interval_ms: 3_600_000 } }, { status: 200, body: { interval_ms: null } }],
    });
    render(<MonitoringSettings target={SERVER} canEdit />);

    const builtin = await screen.findByRole("combobox", { name: "How often the built-in checks run" });
    expect(builtin).toHaveDisplayValue("every 1 min (built-in)");
    expect(screen.getByRole("region", { name: "What is checked" })).toHaveTextContent(/Built-in checks run.*on every server of paris\./);
    await user.selectOptions(builtin, "every 1 h");
    await vi.waitFor(() => expect(bodyOf(fetchMock, "PUT /api/sites/site-1/check-interval")).toEqual({ interval_ms: 3_600_000 }));
  });

  it("changes how often one check runs on this server, and goes back to the site's", async () => {
    const user = userEvent.setup();
    const own = { id: "own-disk", site_id: null, server_id: "srv-1", kind: "disk", name: "disk", instance: "", interval_ms: 3_600_000, timeout_ms: null, params: {}, enabled: true };
    const fetchMock = stubFetch({
      ...serverRoutes([disk()], [sensor({}), port], [], [sensor({})]),
      "POST /api/servers/srv-1/sensor_configs": { status: 201, body: {} },
    });
    const { unmount } = render(<MonitoringSettings target={SERVER} canEdit />);

    // Inherited: the site's value, which a choice copies to this server.
    const often = await screen.findByRole("combobox", { name: "How often to check Disk space" });
    expect(often).toHaveDisplayValue("every 1 min (site default)");
    await user.selectOptions(often, "every 1 h");
    await vi.waitFor(() =>
      expect(bodyOf(fetchMock, "POST /api/servers/srv-1/sensor_configs")).toEqual({
        kind: "disk",
        name: "disk",
        instance: "",
        interval_ms: 3_600_000,
        params: {},
        enabled: true,
      }),
    );
    // A check of this server only has nothing to go back to.
    const portOften = screen.getByRole("combobox", { name: "How often to check Port 443" });
    expect(within(portOften).queryByText(/site default/)).not.toBeInTheDocument();
    unmount();

    // With its own row: the row changes, and going back deletes it.
    const second = stubFetch({
      ...serverRoutes([disk()], [sensor({ origin: "server", entry_id: "own-disk", interval_ms: 3_600_000 })], [own], [sensor({})]),
      "PUT /api/sensor_configs/own-disk": { status: 200, body: {} },
      "DELETE /api/sensor_configs/own-disk": { status: 204 },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);
    const mine = await screen.findByRole("combobox", { name: "How often to check Disk space" });
    expect(mine).toHaveDisplayValue("every 1 h");
    // The built-in disk alert lasts 5 min: say what hourly checks make of it.
    expect(screen.getByText("Checked every 1 h, so this alert opens up to 2 h after the problem starts.")).toBeInTheDocument();
    await user.selectOptions(mine, "every 15 min");
    await vi.waitFor(() =>
      expect(bodyOf(second, "PUT /api/sensor_configs/own-disk")).toEqual({
        kind: "disk",
        name: "disk",
        instance: "",
        interval_ms: 900_000,
        timeout_ms: null,
        params: {},
        enabled: true,
      }),
    );
    await user.selectOptions(screen.getByRole("combobox", { name: "How often to check Disk space" }), "every 1 min (site default)");
    await vi.waitFor(() =>
      expect(second.mock.calls.some(([input, init]) => init?.method === "DELETE" && String(input) === "/api/sensor_configs/own-disk")).toBe(true),
    );
  });

  it("shows intervals as text without a role", async () => {
    stubFetch(serverRoutes([disk()], [sensor({ interval_ms: 30_000 })]));
    render(<MonitoringSettings target={SERVER} canEdit={false} />);
    const checks = await screen.findByRole("region", { name: "What is checked" });
    expect(within(checks).getByText("Built-in checks run every 1 min on every server of paris.")).toBeInTheDocument();
    expect(within(checks).getByText("· every 30 s")).toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });
});
