import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EffectiveSensors, ServerDetail } from "@/lib/api";
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
const open = incident({ server_id: ID, opened_at: new Date(2026, 9, 22, 11, 40).toISOString(), peak_value: 0.946 });
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
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Fleet/web-1/Disk used /var");
    expect(screen.getByText(/on web-1 · disk sensor · every 30 s/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open incident" })).toHaveAttribute("href", expect.stringContaining(open.id));

    expect(screen.getByRole("group", { name: "Summary" })).toHaveTextContent(
      /Current94\.2%\s*Peak, 6 h94\.2%\s*Average, 6 h92\.1%\s*Above critical for2 h 52 min/,
    );
    const chart = screen.getByRole("figure", { name: "History" });
    expect(within(chart).getAllByTestId("chart-threshold").map((l) => l.textContent)).toEqual(["Warning 80%", "Critical 90%"]);
    expect(within(chart).getByTestId("chart-marker")).toHaveTextContent("Opened 11:40");
    expect(within(chart).getByTestId("chart-band")).toBeInTheDocument();
    expect(within(chart).getByRole("button", { name: "Download CSV" })).toBeInTheDocument();
    expect(rangeCalls(fetchMock)[0]).toMatchObject({ metric: "monitorflow_disk_used_ratio", instance: "/var", step: "120" });

    const configuration = screen.getByRole("region", { name: "Configuration" });
    expect(configuration).toHaveTextContent(/Kind\s*disk\s*Instance\s*\/var\s*Interval\s*30 s\s*Timeout\s*10 s/);
    expect(configuration).toHaveTextContent(/Warning\s*> 80%\s*Critical\s*> 90%\s*Duration\s*5 min/);
    expect(configuration).toHaveTextContent(/Hysteresis\s*2% below the threshold before resolving/);
    expect(configuration).toHaveTextContent(/Origin\s*This server · revision 7 · thresholds from the site/);
    expect(configuration).toHaveTextContent(/Agent runs\s*Revision 6 configuration out of date/);

    const incidents = screen.getByRole("region", { name: "Incidents on this sensor" });
    expect(incidents).toHaveTextContent("1 open · 2 in 30 days");
    const rows = within(incidents).getAllByRole("row").slice(1);
    expect(rows.map((r) => r.textContent)).toEqual([
      "Critical open11:40 today2 h 52 min94.6%",
      "Resolved was Warning14 Oct 03:1048 min83.9%",
    ]);
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
