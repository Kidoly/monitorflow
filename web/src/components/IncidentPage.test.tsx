import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { IncidentDetail } from "@/lib/api";
import { setUnauthorizedHandler } from "@/lib/api";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { REFRESH_MS } from "./FleetOverview";
import { IncidentPage, snapshotLines } from "./IncidentPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const NOW = new Date("2026-10-19T12:00:00Z");

const resolved: IncidentDetail = {
  ...incident({ state: "resolved", severity: "warning", resolved_at: "2026-10-19T11:40:00Z" }),
  events: [
    { id: 1, kind: "opened", severity: "warning", value: 0.87, occurred_at: "2026-10-19T11:00:00Z", recorded_at: "2026-10-19T11:00:01Z", details: {} },
    { id: 2, kind: "escalated", severity: "critical", value: 0.96, occurred_at: "2026-10-19T11:10:00Z", recorded_at: "2026-10-19T11:10:01Z", details: {} },
    // Server clock after a restart: earlier than the event before it, still listed after it.
    { id: 3, kind: "deescalated", severity: "warning", value: 0.9, occurred_at: "2026-10-19T11:05:00Z", recorded_at: "2026-10-19T11:30:00Z", details: {} },
    { id: 4, kind: "resolved", severity: null, value: null, occurred_at: "2026-10-19T11:40:00Z", recorded_at: "2026-10-19T11:40:00Z", details: { reason: "threshold_removed" } },
  ],
};
const ID = resolved.id;

const REMEDIATION = `GET /api/remediation/incidents?alert_incident_id=${ID}&limit=1`;
const none = { status: 200, body: { incidents: [], next_cursor: null } };

function stubIncident(detail: IncidentDetail | { status: number; body: unknown }, remediation: { status: number; body: unknown } = none) {
  const fetchMock = stubFetch({
    [`GET /api/incidents/${ID}`]: "events" in detail ? { status: 200, body: detail } : detail,
    [REMEDIATION]: remediation,
  });
  fetchMock.mockImplementation(
    ((original) => async (input, init) => {
      if (String(input).startsWith("/api/metrics/query_range")) {
        const series = [{ labels: { instance: "/var" }, points: [[Date.parse("2026-10-19T11:30:00Z") / 1000, 0.9]] }];
        return new Response(JSON.stringify({ query: "…", series }), { status: 200 });
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

const clock = (iso: string) => new Date(iso).toTimeString().slice(0, 5);

describe("IncidentPage", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl(`/incidents/view/?id=${ID}`);
  });
  afterEach(() => vi.useRealTimers());

  it("shows the incident, its threshold snapshot and its timeline in API order", async () => {
    const fetchMock = stubIncident(resolved);
    render(<IncidentPage id={ID} />);
    expect(screen.getByRole("status", { name: "Loading incident" })).toBeInTheDocument();

    const heading = await screen.findByRole("heading", { level: 1 });
    expect(heading).toHaveTextContent("Disk used /var on web-1");
    expect(within(heading).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/servers\/view\/?\?id=/),
    );
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Incidents/#1d6a9a3e");
    expect(screen.getByText(/was Warning · paris/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open sensor" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/servers\/sensor\/?\?id=.*&sensor=disk_used_ratio&instance=%2Fvar$/),
    );
    expect(screen.getByRole("group", { name: "Summary" })).toHaveTextContent(
      new RegExp(`Last value96\\.2%\\s*Peak97\\.1%\\s*Opened${clock("2026-10-19T11:00:00Z")}\\s*Lasted40 min`),
    );

    const threshold = screen.getByRole("region", { name: "Threshold when it opened" });
    expect(threshold).toHaveTextContent(/Comparison\s*>\s*Warning\s*85%\s*Critical\s*95%\s*Duration\s*5 min/);
    expect(threshold).toHaveTextContent(/Hysteresis\s*2% below the threshold before resolving/);
    expect(threshold).toHaveTextContent(/Resolves when\s*the value is at or below 83%/);

    const timeline = within(screen.getByRole("region", { name: "Timeline" })).getAllByRole("row").slice(1);
    expect(timeline.map((row) => row.textContent?.replace(/^\d\d:\d\d:\d\d/, ""))).toEqual([
      "Opened as Warning87%",
      "Escalated to Critical96%",
      "De-escalated to Warning90%",
      "Resolved · threshold removed—",
    ]);

    const chart = screen.getByRole("figure", { name: "Around the incident" });
    expect(within(chart).getAllByTestId("chart-threshold").map((l) => l.textContent)).toEqual(["Warning 85%", "Critical 95%"]);
    expect(within(chart).getAllByTestId("chart-marker").map((m) => m.textContent)).toEqual([
      `Opened ${clock("2026-10-19T11:00:00Z")}`,
      `Resolved ${clock("2026-10-19T11:40:00Z")}`,
    ]);
    expect(rangeCalls(fetchMock)[0]).toMatchObject({
      metric: "monitorflow_disk_used_ratio",
      instance: "/var",
      start: "2026-10-19T08:00:00.000Z",
      end: "2026-10-19T12:00:00.000Z",
      step: "120",
    });
    expect(screen.queryByText("AI remediation")).not.toBeInTheDocument();
    await vi.waitFor(() => expect(document.title).toBe("Disk used /var on web-1 · MonitorFlow"));
  });

  it("widens the window through the URL", async () => {
    const fetchMock = stubIncident(resolved);
    render(<IncidentPage id={ID} />);
    await screen.findByRole("figure", { name: "Around the incident" });
    await userEvent.click(screen.getByRole("button", { name: "± 12 h" }));
    expect(window.location.search).toBe(`?id=${ID}&window=12h`);
    await vi.waitFor(() => expect(rangeCalls(fetchMock)).toHaveLength(2));
    expect(rangeCalls(fetchMock)[1]).toMatchObject({ start: "2026-10-18T23:00:00.000Z" });
  });

  it("shows what the AI remediation loop did, when it did something", async () => {
    stubIncident(resolved, {
      status: 200,
      body: {
        incidents: [{ id: "r1", server_id: "s", hostname: "web-1", alert_incident_id: ID, metric: "m", instance: "", state: "awaiting_approval", opened_at: "2026-10-19T11:00:20Z", closed_at: null, max_commands: 3, max_duration_s: 3600, event_count: 4 }],
        next_cursor: null,
      },
    });
    render(<IncidentPage id={ID} />);
    const line = (await screen.findByText("AI remediation")).parentElement!;
    expect(line).toHaveTextContent(new RegExp(`AI remediation\\s*Awaiting approval\\s*4 events · opened ${clock("2026-10-19T11:00:20Z")}`));
  });

  it("refreshes an open incident and stops once it is resolved", async () => {
    const open = { ...resolved, ...incident(), events: resolved.events.slice(0, 1) };
    const fetchMock = stubIncident(open);
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText(/^open · paris/)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Summary" })).toHaveTextContent(/Duration1 h/);
    expect(screen.getAllByTestId("chart-marker").map((m) => m.textContent)).toEqual([`Opened ${clock("2026-10-19T11:00:00Z")}`, "Now · open"]);

    fetchMock.mockClear();
    stubIncident(resolved);
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByText(/was Warning/)).toBeInTheDocument();
    const calls = vi.mocked(fetch).mock.calls.length;
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS * 2));
    expect(vi.mocked(fetch).mock.calls.length).toBe(calls);
  });

  it("says when the incident does not exist", async () => {
    stubIncident({ status: 404, body: { error: "not found" } });
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText("Incident not found")).toBeInTheDocument();
  });

  it("does not call the API without a valid ID", () => {
    const fetchMock = stubFetch({});
    render(<IncidentPage id="../servers" />);
    expect(screen.getByText("Incident not found")).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows backend errors and leaves a 401 to the login redirect", async () => {
    stubIncident({ status: 500, body: { error: "internal error" } });
    const { unmount } = render(<IncidentPage id={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
    unmount();

    stubIncident({ status: 401, body: { error: "authentication required" } });
    const toLogin = vi.fn();
    const restore = setUnauthorizedHandler(toLogin);
    render(<IncidentPage id={ID} />);
    await vi.waitFor(() => expect(toLogin).toHaveBeenCalledOnce());
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    restore();
  });
});

describe("snapshotLines", () => {
  it("draws the values the snapshot has", () => {
    expect(snapshotLines({ ...incident().threshold, warning: null })).toEqual([
      { severity: "critical", value: 0.95, label: "Critical" },
    ]);
  });
});
