import { act, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { IncidentDetail } from "@/lib/api";
import { setUnauthorizedHandler } from "@/lib/api";
import { stubFetch } from "@/test/fetch";
import { incident } from "@/test/incidents";

import { REFRESH_MS } from "./FleetOverview";
import { IncidentPage, snapshotLines } from "./IncidentPage";

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

function stubIncident(detail: IncidentDetail | { status: number; body: unknown }) {
  const fetchMock = stubFetch({
    [`GET /api/incidents/${ID}`]: "events" in detail ? { status: 200, body: detail } : detail,
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

describe("IncidentPage", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
  });
  afterEach(() => vi.useRealTimers());

  it("shows the incident, its threshold snapshot and its timeline in API order", async () => {
    const fetchMock = stubIncident(resolved);
    render(<IncidentPage id={ID} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading incident…");

    const heading = await screen.findByRole("heading", { level: 1 });
    expect(heading).toHaveTextContent("Disk used/var on web-1");
    expect(within(heading).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/servers\/view\/?\?id=/),
    );
    expect(screen.getByRole("banner")).toHaveTextContent(/Lasted\s*40 min/);
    const threshold = screen.getByRole("region", { name: "Threshold when it opened" });
    expect(threshold).toHaveTextContent(/Fires when\s*above the threshold \(>\)/);
    expect(threshold).toHaveTextContent(/Warning\s*85%\s*Critical\s*95%\s*For\s*5 min\s*Hysteresis\s*2%/);
    expect(threshold).toHaveTextContent(/Applies to\s*every instance/);

    const timeline = within(screen.getByRole("region", { name: "Timeline" })).getAllByRole("listitem");
    expect(timeline.map((li) => li.textContent)).toEqual([
      expect.stringMatching(/Opened▲ Warningat 87%$/),
      expect.stringMatching(/Escalated◆ Criticalat 96%$/),
      expect.stringMatching(/De-escalated▲ Warningat 90%$/),
      expect.stringMatching(/Resolved\(threshold removed\)$/),
    ]);

    const chart = screen.getByRole("region", { name: "Around the incident" });
    expect(within(chart).getAllByTestId("reference-line").map((l) => l.textContent)).toEqual([
      "Warning 85%",
      "Critical 95%",
    ]);
    expect(within(chart).getAllByTestId("time-marker").map((m) => m.textContent)).toEqual(["Opened", "Resolved"]);
    const range = new URL(String(fetchMock.mock.calls[1]![0]), "http://localhost").searchParams;
    expect(Object.fromEntries(range)).toMatchObject({
      metric: "monitorflow_disk_used_ratio",
      instance: "/var",
      start: "2026-10-19T10:45:00.000Z",
      end: "2026-10-19T11:55:00.000Z",
      step: "30",
    });
    await vi.waitFor(() => expect(document.title).toBe("Disk used on web-1 · MonitorFlow"));
  });

  it("refreshes an open incident and stops once it is resolved", async () => {
    const open = { ...resolved, ...incident(), events: resolved.events.slice(0, 1) };
    const fetchMock = stubIncident(open);
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText("Open")).toBeInTheDocument();
    expect(screen.getByRole("banner")).toHaveTextContent(/Open for\s*1 h/);
    expect(screen.getAllByTestId("time-marker").map((m) => m.textContent)).toEqual(["Opened"]);

    fetchMock.mockClear();
    stubIncident(resolved);
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByText("Resolved", { selector: "span" })).toBeInTheDocument();
    const calls = vi.mocked(fetch).mock.calls.length;
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS * 2));
    expect(vi.mocked(fetch).mock.calls.length).toBe(calls);
  });

  it("says when the incident does not exist", async () => {
    stubIncident({ status: 404, body: { error: "not found" } });
    render(<IncidentPage id={ID} />);
    expect(await screen.findByRole("heading", { name: "Incident not found" })).toBeInTheDocument();
  });

  it("does not call the API without a valid ID", () => {
    const fetchMock = stubFetch({});
    render(<IncidentPage id="../servers" />);
    expect(screen.getByRole("heading", { name: "Incident not found" })).toBeInTheDocument();
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
