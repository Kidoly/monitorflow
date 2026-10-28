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

/** The GET routes of the server panel. */
function serverRoutes(thresholds: EffectiveThreshold[], sensors: EffectiveSensor[], rows: unknown[] = []) {
  return {
    "GET /api/servers/srv-1/thresholds/effective": { status: 200, body: { server_id: "srv-1", site_id: "site-1", revision: 1, thresholds } },
    "GET /api/servers/srv-1/sensors": {
      status: 200,
      body: { server_id: "srv-1", site_id: "site-1", revision: 1, applied_revision: 1, applied_at: null, in_sync: true, sensors },
    },
    "GET /api/servers/srv-1/sensor_configs": { status: 200, body: { entries: rows } },
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
      "GET /api/sites/site-1/sensors": { status: 200, body: { site_id: "site-1", sensors: [sensor({})] } },
      "GET /api/sites/site-1/sensor_configs": { status: 200, body: { entries: [] } },
      "POST /api/sites/site-1/sensor_configs": [{ status: 409, body: { error: "exists" } }, { status: 201, body: {} }],
      "GET /api/sites/site-1/thresholds": { status: 200, body: { entries: [] } },
      "POST /api/sites/site-1/thresholds": { status: 201, body: {} },
    });
    render(<MonitoringSettings target={SITE} canEdit />);

    await user.click(await screen.findByRole("button", { name: "Add a check" }));
    const form = screen.getByRole("form", { name: "Add a check" });
    // A site can mix systems, so every kind is offered.
    expect(within(form).getAllByRole("option")).toHaveLength(6);
    expect(within(form).getByLabelText("Host")).toHaveValue("127.0.0.1");
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
      interval_ms: 60_000,
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
});
