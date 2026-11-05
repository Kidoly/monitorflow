import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { DashboardData, Panel, ScreenView } from "@/lib/api";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { DisplayScreen } from "./DisplayScreen";

vi.mock("next/navigation", () => import("@/test/navigation"));

const SECRET = "ab".repeat(32);
const NOW = 1_800_000_000;

const figure: Panel = {
  id: "f",
  title: "Average CPU",
  kind: "figure",
  width: 1,
  servers: { kind: "all" },
  metric: "monitorflow_cpu_usage_ratio",
  combine: "avg",
};

const alerts: Panel = { id: "a", title: "Open alerts", kind: "alerts", width: 2, servers: { kind: "all" }, count: 5 };

const VIEW: ScreenView = {
  name: "Reception TV",
  seconds_per_page: 10,
  refresh_seconds: 30,
  expires_at: null,
  dashboards: [
    { id: "d1", name: "NOC", spec: { version: 1, range: "1h", panels: [figure, alerts] } },
    { id: "d2", name: "Disks", spec: { version: 1, range: "1h", panels: [figure] } },
  ],
};

function data(value: number): DashboardData {
  return {
    start: NOW - 3600,
    end: NOW,
    step: 30,
    at: NOW,
    now: NOW,
    panels: [
      { id: "f", servers: 3, unavailable: 0, kind: "figure", value },
      {
        id: "a",
        servers: 3,
        unavailable: 0,
        kind: "alerts",
        alerts: [
          {
            id: "i1",
            server_id: "s1",
            hostname: "db1",
            metric: "monitorflow_disk_used_ratio",
            instance: "/",
            severity: "critical",
            opened_at: new Date((NOW - 600) * 1000).toISOString(),
          },
        ],
      },
    ],
  };
}

const ok = (body: unknown) => ({ status: 200, body });

beforeEach(() => {
  vi.useFakeTimers({ shouldAdvanceTime: true, toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date", "performance"] });
  setUrl("/display/");
  window.location.hash = `#${SECRET}`;
});

afterEach(() => {
  vi.useRealTimers();
  vi.unstubAllGlobals();
  window.location.hash = "";
});

describe("DisplayScreen with a display link", () => {
  it("trades the secret once, shows the dashboard without any link, and cycles", async () => {
    const fetchMock = stubFetch({
      "POST /api/display/start": ok(VIEW),
      "POST /api/display/data": ok(data(0.42)),
      "GET /api/display": ok(VIEW),
    });
    const { container } = render(<DisplayScreen />);
    expect(await screen.findByText("42%")).toBeInTheDocument();
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("NOC");
    expect(screen.getByText("1 of 2")).toBeInTheDocument();
    expect(screen.getByText(/Updated/)).toBeInTheDocument();
    expect(screen.getByText("Critical")).toBeInTheDocument();
    // Nothing on a screen leads anywhere.
    expect(container.querySelectorAll("a")).toHaveLength(0);
    const start = fetchMock.mock.calls.find((c) => String(c[0]) === "/api/display/start");
    expect(JSON.parse(String(start?.[1]?.body))).toEqual({ token: SECRET });
    expect(fetchMock.mock.calls.every((c) => !String(c[0]).includes(SECRET))).toBe(true);

    await act(() => vi.advanceTimersByTimeAsync(10_000));
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("Disks");
    expect(screen.getByText("2 of 2")).toBeInTheDocument();
    const bodies = fetchMock.mock.calls.filter((c) => String(c[0]) === "/api/display/data").map((c) => JSON.parse(String(c[1]?.body)));
    expect(bodies).toContainEqual({ dashboard_id: "d2" });
  });

  it("says plainly when the data is more than a minute old", async () => {
    stubFetch({
      "POST /api/display/start": ok(VIEW),
      "POST /api/display/data": [ok(data(0.42)), new TypeError("Failed to fetch")],
      "GET /api/display": new TypeError("Failed to fetch"),
    });
    render(<DisplayScreen />);
    expect(await screen.findByText("42%")).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(59_000));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(3_000));
    expect(screen.getByRole("alert")).toHaveTextContent(/^Data from \d\d:\d\d — this screen has lost its connection to MonitorFlow\./);
    expect(screen.getByRole("main")).toHaveAttribute("data-stale");
  });

  it("opens the link again when the screen session ends", async () => {
    const fetchMock = stubFetch({
      "POST /api/display/start": ok(VIEW),
      "POST /api/display/data": [{ status: 401, body: { error: "authentication required" } }, ok(data(0.5))],
    });
    render(<DisplayScreen />);
    expect(await screen.findByText("50%")).toBeInTheDocument();
    expect(fetchMock.mock.calls.filter((c) => String(c[0]) === "/api/display/start")).toHaveLength(2);
  });

  it("says why a link no longer works, instead of showing old numbers", async () => {
    stubFetch({ "POST /api/display/start": { status: 410, body: { error: "gone" } } });
    render(<DisplayScreen />);
    expect(await screen.findByText("This display link no longer works")).toBeInTheDocument();
    expect(screen.getByText(/Ask an administrator for a new one/)).toBeInTheDocument();
  });

  it("says when its maker can no longer show its dashboards", async () => {
    stubFetch({ "POST /api/display/start": { status: 403, body: { error: "forbidden" } } });
    render(<DisplayScreen />);
    expect(await screen.findByText("Nothing to show")).toBeInTheDocument();
  });
});

describe("DisplayScreen signed in", () => {
  it("shows one dashboard with the user's session", async () => {
    window.location.hash = "";
    setUrl("/display/?id=d1");
    const fetchMock = stubFetch({
      "GET /api/dashboards/d1": ok({
        id: "d1",
        name: "NOC",
        shared_with: { kind: "site", id: "s1", name: "paris" },
        panels: 1,
        updated_at: "2026-11-01T10:00:00Z",
        updated_by_username: null,
        can_edit: true,
        can_link: false,
        spec: { version: 1, range: "6h", panels: [figure] },
        revision: 1,
      }),
      "POST /api/dashboard-data": ok(data(0.1)),
    });
    render(<DisplayScreen />);
    expect(await screen.findByText("10%")).toBeInTheDocument();
    const call = fetchMock.mock.calls.find((c) => String(c[0]) === "/api/dashboard-data");
    expect(JSON.parse(String(call?.[1]?.body))).toMatchObject({ range: "6h", within: { kind: "site", id: "s1" } });
  });
});
