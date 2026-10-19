import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { EffectiveSensor, MetricList, QueryResult } from "@/lib/api";

import { REFRESH_MS } from "./FleetOverview";
import { RANGES, ServerGraphs, chartWindow, planCharts } from "./ServerGraphs";

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
// 12:00:10 UTC: 1 h ranges end at 12:00:00, the last 30 s step.
const NOW = new Date("2026-10-19T12:00:10Z");
const END = Date.parse("2026-10-19T12:00:00Z") / 1000;

const discovery: MetricList = {
  truncated: false,
  metrics: [
    { name: "monitorflow_disk_used_ratio", instances: ["/", "/var"] },
    { name: "monitorflow_port_open", instances: [] },
    { name: "monitorflow_cpu_usage_ratio", instances: ["0", "1", "total"] },
  ],
};

function result(series: Record<string, number>): QueryResult {
  return {
    query: "…",
    series: Object.entries(series).map(([instance, v]) => ({
      labels: { instance },
      points: [
        [END - 60, v / 2],
        [END, v],
      ],
    })),
  };
}

type Reply = { status: number; body: unknown };

/** Stubs fetch by path; query_range replies are picked by the `metric` parameter. */
function stubMetrics(metrics: Reply | Reply[], queries: Record<string, Reply>) {
  const discoveryReplies = Array.isArray(metrics) ? [...metrics] : [metrics];
  const fetchMock = vi.fn<typeof fetch>(async (input) => {
    const url = new URL(String(input), "http://localhost");
    let reply: Reply | undefined;
    if (url.pathname === `/api/servers/${ID}/metrics`) {
      reply = discoveryReplies.length > 1 ? discoveryReplies.shift() : discoveryReplies[0];
    } else if (url.pathname === "/api/metrics/query_range") {
      reply = queries[url.searchParams.get("metric") ?? ""];
    }
    if (!reply) throw new Error(`unexpected request ${url}`);
    return new Response(JSON.stringify(reply.body), { status: reply.status });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function rangeParams(fetchMock: ReturnType<typeof stubMetrics>) {
  return fetchMock.mock.calls
    .map(([input]) => new URL(String(input), "http://localhost"))
    .filter((url) => url.pathname === "/api/metrics/query_range")
    .map((url) => Object.fromEntries(url.searchParams));
}

const ok = (body: unknown): Reply => ({ status: 200, body });

describe("ServerGraphs", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
  });
  afterEach(() => vi.useRealTimers());

  it("queries each discovered metric over the selected range", async () => {
    const fetchMock = stubMetrics(ok(discovery), {
      monitorflow_cpu_usage_ratio: ok(result({ total: 0.42 })),
      monitorflow_disk_used_ratio: ok(result({ "/": 0.5, "/var": 0.25 })),
      monitorflow_port_open: ok(result({ "": 1 })),
    });
    render(<ServerGraphs serverId={ID} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading metrics…");

    const figures = await screen.findAllByRole("figure");
    expect(figures.map((f) => within(f).getByText(/./, { selector: "figcaption" }).textContent)).toEqual([
      "CPU usage",
      "Disk used",
      "port_open",
    ]);
    expect(figures[0]).toHaveTextContent("42% total");
    expect(figures[1]).toHaveTextContent(/50% \/.*25% \/var/);
    expect(within(figures[2]!).getByRole("listitem")).toHaveTextContent(/^1$/);

    const start = new Date((END - 3600) * 1000).toISOString();
    const end = new Date(END * 1000).toISOString();
    expect(rangeParams(fetchMock)).toEqual([
      { server_id: ID, metric: "monitorflow_cpu_usage_ratio", instance: "total", start, end, step: "30" },
      { server_id: ID, metric: "monitorflow_disk_used_ratio", start, end, step: "30" },
      { server_id: ID, metric: "monitorflow_port_open", start, end, step: "30" },
    ]);
  });

  it("reloads with the range's step when the range changes, and keeps refreshing", async () => {
    const fetchMock = stubMetrics(ok({ truncated: false, metrics: [discovery.metrics[0]] }), {
      monitorflow_disk_used_ratio: ok(result({ "/": 0.5 })),
    });
    render(<ServerGraphs serverId={ID} />);
    await screen.findByRole("figure");
    await userEvent.click(screen.getByRole("button", { name: "7 d" }));
    expect(screen.getByRole("button", { name: "7 d" })).toHaveAttribute("aria-pressed", "true");
    await vi.waitFor(() => expect(rangeParams(fetchMock)).toHaveLength(2));
    const week = rangeParams(fetchMock)[1]!;
    const weekEnd = Math.floor(NOW.getTime() / 1000 / 1800) * 1800;
    expect(week).toMatchObject({
      step: "1800",
      rollup: "max",
      start: new Date((weekEnd - 604_800) * 1000).toISOString(),
      end: new Date(weekEnd * 1000).toISOString(),
    });

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(rangeParams(fetchMock)).toHaveLength(3);
  });

  it("rolls up long ranges per metric, with steps of at least the sensor's interval", async () => {
    const metrics = ["cpu_usage_ratio", "memory_available_ratio", "disk_total_bytes", "backup_status"];
    const fetchMock = stubMetrics(
      ok({ truncated: false, metrics: metrics.map((m) => ({ name: `monitorflow_${m}`, instances: [] })) }),
      Object.fromEntries(metrics.map((m) => [`monitorflow_${m}`, ok(result({ "": 1 }))])),
    );
    const sensor = (name: string, interval_ms: number): EffectiveSensor => ({
      origin: "server",
      entry_id: null,
      kind: name,
      name,
      instance: "",
      interval_ms,
      timeout_ms: null,
      params: {},
    });
    render(<ServerGraphs serverId={ID} sensors={[sensor("cpu", 60_000), sensor("backup", 3_600_000)]} />);
    await screen.findAllByRole("figure");
    // Short ranges stay raw.
    expect(rangeParams(fetchMock).every((p) => !("rollup" in p))).toBe(true);
    await userEvent.click(screen.getByRole("button", { name: "6 h" }));
    await vi.waitFor(() => expect(rangeParams(fetchMock)).toHaveLength(8));
    expect(rangeParams(fetchMock).every((p) => !("rollup" in p))).toBe(true);

    await userEvent.click(screen.getByRole("button", { name: "24 h" }));
    await vi.waitFor(() => expect(rangeParams(fetchMock)).toHaveLength(12));
    const day = Object.fromEntries(rangeParams(fetchMock).slice(8).map((p) => [p.metric, p]));
    expect(day["monitorflow_cpu_usage_ratio"]).toMatchObject({ rollup: "max", step: "300" });
    expect(day["monitorflow_memory_available_ratio"]).toMatchObject({ rollup: "min", step: "300" });
    expect(day["monitorflow_disk_total_bytes"]).toMatchObject({ rollup: "avg", step: "300" });
    // An hourly script: one point per hour, ending on the hour.
    const hourEnd = Math.floor(NOW.getTime() / 1000 / 3600) * 3600;
    expect(day["monitorflow_backup_status"]).toMatchObject({
      rollup: "max",
      step: "3600",
      end: new Date(hourEnd * 1000).toISOString(),
      start: new Date((hourEnd - 86_400) * 1000).toISOString(),
    });
  });

  it("keeps every range within the proxy's point limit", () => {
    for (const range of RANGES) {
      expect(range.seconds / range.step).toBeLessThanOrEqual(500);
    }
  });

  it("explains an empty server", async () => {
    stubMetrics(ok({ truncated: false, metrics: [] }), {});
    render(<ServerGraphs serverId={ID} />);
    expect(await screen.findByText(/No metrics yet/)).toHaveTextContent("about 30 seconds");
    expect(screen.queryByRole("figure")).not.toBeInTheDocument();
  });

  it("explains a chart with no points in the range", async () => {
    stubMetrics(ok({ truncated: false, metrics: [discovery.metrics[0]] }), {
      monitorflow_disk_used_ratio: ok({ query: "…", series: [] }),
    });
    render(<ServerGraphs serverId={ID} />);
    expect(await screen.findByText(/No data in this range yet/)).toBeInTheDocument();
  });

  it("shows backend errors per chart and for discovery", async () => {
    stubMetrics([{ status: 502, body: { error: "metrics backend unavailable" } }, ok(discovery)], {
      monitorflow_cpu_usage_ratio: { status: 504, body: { error: "metrics backend timed out" } },
      monitorflow_disk_used_ratio: ok(result({ "/": 0.5 })),
      monitorflow_port_open: ok(result({ "": 1 })),
    });
    render(<ServerGraphs serverId={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("metrics backend is unavailable");

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    const cpu = (await screen.findAllByRole("figure"))[0]!;
    expect(within(cpu).getByRole("alert")).toHaveTextContent("timed out. Try a shorter range.");
    expect(screen.getAllByRole("alert")).toHaveLength(1);
  });

  it("passes reference lines through to the charts", async () => {
    stubMetrics(ok({ truncated: false, metrics: [discovery.metrics[0]] }), {
      monitorflow_disk_used_ratio: ok(result({ "/": 0.5 })),
    });
    render(
      <ServerGraphs
        serverId={ID}
        referenceLines={(plan) => [{ value: 0.85, label: `Warning ${plan.metric}`, severity: "warning" }]}
      />,
    );
    expect(await screen.findByTestId("reference-line")).toHaveTextContent(
      "Warning monitorflow_disk_used_ratio 85%",
    );
  });
});

describe("chartWindow", () => {
  const [hour, , day, week] = RANGES;
  const now = END + 10;

  it("keeps short ranges raw and long ones rolled up", () => {
    expect(chartWindow(hour, "monitorflow_cpu_usage_ratio", 600, now)).toEqual({ start: END - 3600, end: END, step: 30 });
    expect(chartWindow(day, "monitorflow_cpu_usage_ratio", 60, now)).toMatchObject({ step: 300, rollup: "max" });
    expect(chartWindow(week, "monitorflow_port_up", 60, now)).toMatchObject({ step: 1800, rollup: "min" });
  });

  it("rounds a long interval up to a multiple of the range's step", () => {
    const window = chartWindow(day, "monitorflow_cpu_usage_ratio", 420, now);
    expect(window.step).toBe(600);
    expect(window.end % 600).toBe(0);
    expect(window.end - window.start).toBe(86_400);
  });
});

describe("planCharts", () => {
  it("orders built-in metrics first and titles them", () => {
    const plans = planCharts([
      { name: "monitorflow_zz_custom", instances: [] },
      { name: "monitorflow_memory_used_bytes", instances: [] },
      { name: "monitorflow_cpu_usage_ratio", instances: ["0"] },
      { name: "monitorflow_cpu_load1", instances: [] },
    ]);
    expect(plans).toEqual([
      { metric: "monitorflow_cpu_usage_ratio", title: "CPU usage" },
      { metric: "monitorflow_cpu_load1", title: "Load average, 1 min" },
      { metric: "monitorflow_memory_used_bytes", title: "Memory used" },
      { metric: "monitorflow_zz_custom", title: "zz_custom" },
    ]);
  });
});
