import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { EffectiveSensor, EffectiveThreshold } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { MonitoringSettings, type Target } from "./MonitoringSettings";

vi.mock("next/navigation", () => import("@/test/navigation"));

const SERVER: Target = { kind: "server", id: "srv-1", siteId: "site-1", siteName: "paris", osFamily: "debian" };
const DISK = "monitorflow_disk_used_ratio";
const INTERVAL = { check_interval_ms: 60_000, check_interval_origin: "default" };
const HOUR = 3_600_000;

const disk: EffectiveThreshold = {
  origin: "default",
  threshold_id: null,
  metric: DISK,
  instance: null,
  enabled: true,
  kind: "value",
  comparison: ">",
  warning: 0.85,
  critical: 0.95,
  duration_s: 300,
  hysteresis: 0.02,
};

const diskSensor: EffectiveSensor = {
  origin: "default",
  entry_id: null,
  kind: "disk",
  name: "disk",
  instance: "",
  interval_ms: 60_000,
  timeout_ms: null,
  params: {},
};

const forecastRule = {
  metric: DISK,
  instance: null,
  kind: "forecast",
  comparison: ">=",
  warning: 259_200,
  critical: 86_400,
  duration_s: 0,
  hysteresis: 3600,
  enabled: true,
  window_s: 86_400,
  target: 1,
};

function routes(recommended: unknown[] = []) {
  return {
    "GET /api/servers/srv-1/thresholds/effective": {
      status: 200,
      body: { server_id: "srv-1", site_id: "site-1", revision: 1, thresholds: [disk], recommended },
    },
    "GET /api/servers/srv-1/sensors": {
      status: 200,
      body: { server_id: "srv-1", site_id: "site-1", ...INTERVAL, revision: 1, applied_revision: 1, applied_at: null, in_sync: true, sensors: [diskSensor] },
    },
    "GET /api/servers/srv-1/sensor_configs": { status: 200, body: { entries: [] } },
    "GET /api/sites/site-1/sensors": { status: 200, body: { site_id: "site-1", ...INTERVAL, sensors: [diskSensor] } },
  };
}

const bodiesOf = (fetchMock: ReturnType<typeof stubFetch>, key: string) =>
  fetchMock.mock.calls
    .filter(([input, init]) => `${init?.method ?? "GET"} ${String(input)}` === key)
    .map(([, init]) => JSON.parse(String(init?.body)) as Record<string, unknown>);

describe("the richer rule editor", () => {
  it("turns an alert into a forecast with a live preview, and adds it beside the threshold", async () => {
    const user = userEvent.setup();
    const now = Date.now();
    const fetchMock = stubFetch({
      ...routes(),
      "POST /api/servers/srv-1/thresholds/preview": {
        status: 200,
        body: {
          server_id: "srv-1",
          from: now - 72 * HOUR,
          to: now,
          days: 3,
          truncated: false,
          series: [
            {
              instance: "/var",
              points: [[now - 2 * HOUR, 200_000], [now - HOUR, 150_000]],
              incidents: [{ severity: "warning", opened_at: now - 2 * HOUR, resolved_at: null, peak_value: 150_000 }],
            },
          ],
        },
      },
      "GET /api/servers/srv-1/thresholds": {
        status: 200,
        body: { entries: [{ id: "t1", site_id: null, server_id: "srv-1", ...disk, warning: 0.8 }] },
      },
      "POST /api/servers/srv-1/thresholds": { status: 201, body: {} },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const form = screen.getByRole("form", { name: "Edit alert" });
    await user.selectOptions(within(form).getByLabelText("Alert on"), "When it will reach a value");
    expect(within(form).getByText("This adds another alert on the same measure. The current one stays.")).toBeInTheDocument();
    expect(within(form).getByLabelText("Value to reach")).toHaveValue("100");
    expect(within(form).getByLabelText("Warn this long before")).toHaveValue("72");
    // Turning off is for the rule being edited, not the one being added.
    expect(within(form).queryByRole("button", { name: /Turn this alert off/ })).not.toBeInTheDocument();

    const preview = within(form).getByRole("region", { name: "What this rule would have done" });
    expect(await within(preview).findByText("1 incident would have opened.")).toBeInTheDocument();
    expect(within(preview).getByText(/Warning from .*, still open/)).toBeInTheDocument();
    expect(bodiesOf(fetchMock, "POST /api/servers/srv-1/thresholds/preview").at(-1)).toEqual({ rule: forecastRule, days: 3 });

    // Another period replays again.
    await user.click(within(preview).getByRole("button", { name: "7 days" }));
    await waitFor(() => expect(bodiesOf(fetchMock, "POST /api/servers/srv-1/thresholds/preview").at(-1)?.days).toBe(7));

    await user.click(within(form).getByRole("button", { name: "Save" }));
    // The server has a value row on this disk: the forecast is a new row, not an update of it.
    await waitFor(() => expect(bodiesOf(fetchMock, "POST /api/servers/srv-1/thresholds")).toEqual([forecastRule]));
  });

  it("asks nothing it cannot preview, and says why", async () => {
    const user = userEvent.setup();
    stubFetch(routes());
    render(<MonitoringSettings target={SERVER} canEdit />);
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const form = screen.getByRole("form", { name: "Edit alert" });
    await user.selectOptions(within(form).getByLabelText("Alert on"), "When it stops reporting");
    const warn = within(form).getByLabelText("Warn after a silence of");
    await user.clear(warn);
    await user.type(warn, "x");
    expect(within(form).getByText("Fix the fields above to see the preview.")).toBeInTheDocument();
    await user.click(within(form).getByRole("button", { name: "Save" }));
    expect(within(form).getByRole("alert")).toHaveTextContent("Enter a time from 1 minute to 7 days.");
  });

  it("offers the disk forecast for the site, added or declined in one click", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...routes([{ key: "disk_forecast", rule: forecastRule }]),
      "POST /api/sites/site-1/thresholds": { status: 201, body: {} },
    });
    render(<MonitoringSettings target={SERVER} canEdit />);
    const offer = (await screen.findByText("Warn 3 days before a disk is full")).closest("li")!;
    expect(offer).toHaveTextContent("Suggested for every server of paris.");
    await user.click(within(offer).getByRole("button", { name: "No thanks" }));
    await waitFor(() => expect(bodiesOf(fetchMock, "POST /api/sites/site-1/thresholds")).toEqual([{ ...forecastRule, enabled: false }]));
    await user.click(within(offer).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(bodiesOf(fetchMock, "POST /api/sites/site-1/thresholds").at(-1)).toEqual(forecastRule));
  });

  it("hides the buttons of a suggestion from readers", async () => {
    stubFetch(routes([{ key: "disk_forecast", rule: forecastRule }]));
    render(<MonitoringSettings target={SERVER} canEdit={false} />);
    const offer = (await screen.findByText("Warn 3 days before a disk is full")).closest("li")!;
    expect(within(offer).queryByRole("button")).not.toBeInTheDocument();
  });
});
