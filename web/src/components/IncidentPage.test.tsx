import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { IncidentDetail } from "@/lib/api";
import { setUnauthorizedHandler } from "@/lib/api";
import { setActiveLocale } from "@/i18n/locale";
import { As, oneSite, viewer } from "@/test/admin";
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

const REMEDIATION = `GET /api/remediation/incidents?alert_incident_id=${ID}&limit=20`;
const RUN_STATUS = `GET /api/remediation/alerts/${ID}`;
const RUN = `POST /api/remediation/alerts/${ID}/run`;
const none = { status: 200, body: { incidents: [], next_cursor: null } };
const noStatus = { status: 404, body: { error: "not found" } };

type Reply = { status: number; body: unknown };

function stubIncident(
  detail: IncidentDetail | Reply,
  remediation: Reply = none,
  run: Reply | Reply[] = noStatus,
  extra: Record<string, Reply | Reply[]> = {},
) {
  const fetchMock = stubFetch({
    [`GET /api/incidents/${ID}`]: "events" in detail ? { status: 200, body: detail } : detail,
    [REMEDIATION]: remediation,
    [RUN_STATUS]: run,
    ...extra,
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
    expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Incidents›#1d6a9a3e");
    expect(screen.getByText(/was Warning · paris/)).toBeInTheDocument();
    // The host is a link in the title; no second link to it beside.
    expect(screen.getAllByRole("link", { name: /web-1/ })).toHaveLength(1);
    expect(screen.getByRole("link", { name: "Open sensor" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/servers\/sensor\/?\?id=.*&sensor=disk_used_ratio&instance=%2Fvar$/),
    );
    // From 15 minutes before it opened to 15 minutes after it resolved.
    const logs = new URL(screen.getByRole("link", { name: "Logs around this incident" }).getAttribute("href")!, "http://x");
    expect(logs.searchParams.get("tab")).toBe("logs");
    expect(logs.searchParams.get("from")).toBe("2026-10-19T10:45:00.000Z");
    expect(logs.searchParams.get("to")).not.toBeNull();
    expect(screen.getByRole("group", { name: "Summary" })).toHaveTextContent(
      new RegExp(`Last value96\\.2%\\s*Peak97\\.1%\\s*Opened${clock("2026-10-19T11:00:00Z")}\\s*Lasted40 min`),
    );

    const threshold = screen.getByRole("region", { name: "Threshold when it opened" });
    expect(threshold).toHaveTextContent(/Comparison\s*>\s*Warning\s*85%\s*Critical\s*95%\s*Duration\s*5 min/);
    // The margin is worked out in "Resolves when", not said again.
    expect(threshold).not.toHaveTextContent(/Recovery margin/);
    expect(threshold).toHaveTextContent(/Resolves when\s*the value is at or below 83%/);

    const timeline = within(screen.getByRole("region", { name: "Timeline" })).getAllByRole("row").slice(1);
    expect(timeline.map((row) => row.textContent?.replace(/^\d\d:\d\d:\d\d/, ""))).toEqual([
      "Opened as Warning87%",
      "Escalated to Critical96%",
      "De-escalated to Warning90%",
      "Resolved · threshold removed",
    ]);

    const chart = screen.getByRole("figure", { name: "Around the incident" });
    expect(within(chart).getAllByTestId("chart-threshold")).toHaveLength(2);
    expect(chart).toHaveTextContent(/Warning 85%.*Critical 95%/);
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

  it("shows a forecast's time left and expected date, without limits in seconds on the chart", async () => {
    const forecast: IncidentDetail = {
      ...incident({ state: "open", severity: "warning", last_value: 172_800, peak_value: 172_800 }),
      kind: "forecast",
      expected_at: "2026-10-21T12:00:00Z",
      threshold: {
        metric: "monitorflow_disk_used_ratio", instance: null, comparison: ">=", warning: 259_200, critical: 86_400,
        duration_s: 0, hysteresis: 3600, kind: "forecast", window_s: 86_400, target: 1,
      },
      events: [{ id: 1, kind: "opened", severity: "warning", value: 172_800, occurred_at: "2026-10-19T11:00:00Z", recorded_at: "2026-10-19T11:00:01Z", details: {} }],
    };
    stubIncident(forecast);
    render(<IncidentPage id={ID} />);
    const tiles = await screen.findByRole("group", { name: "Summary" });
    expect(within(tiles).getAllByText("2 d left").length).toBeGreaterThan(0);
    expect(tiles).toHaveTextContent(/Expected.*2026-10-21/);
    expect(screen.getByText(/Warn 3 d before .* reaches 100%/)).toBeInTheDocument();
    expect(screen.getByText(/Expected around 2026-10-21/)).toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubIncident(resolved);
    render(<IncidentPage id={ID} />);
    const heading = await screen.findByRole("heading", { level: 1 });
    expect(heading).toHaveTextContent(/ sur web-1$/);
    expect(screen.getByText(/était Avertissement · paris/)).toBeInTheDocument();
    const threshold = screen.getByRole("region", { name: "Seuil à l’ouverture" });
    expect(threshold).toHaveTextContent(/Se résout quand\s*la valeur est inférieure ou égale à 83 %/);
    const timeline = within(screen.getByRole("region", { name: "Chronologie" })).getAllByRole("row").slice(1);
    expect(timeline[1]).toHaveTextContent("Aggravé en Critique");
    expect(screen.getByRole("button", { name: "± 3 j" })).toBeInTheDocument();
  });

  it("says when a notification sent it together with others", async () => {
    const GROUP = "9a3e2c1b-7d4f-4e6a-8b5c-1d2e3f4a5b6c";
    stubIncident({
      ...resolved,
      notification_groups: [
        { id: "11111111-2222-4333-8444-555555555555", incidents: 2, created_at: "2026-10-19T11:00:30Z" },
        { id: GROUP, incidents: 40, created_at: "2026-10-19T11:41:00Z" },
      ],
    });
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText(/Sent together with 39 other incidents in one notification/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Show them" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/incidents/?\\?group=${GROUP}$`)),
    );
  });

  it("says nothing of notifications sent for it alone", async () => {
    stubIncident(resolved);
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText("Timeline")).toBeInTheDocument();
    expect(screen.queryByText(/Sent together/)).not.toBeInTheDocument();
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

  describe("running AI again (ADR-0165)", () => {
    const open = () => ({ ...resolved, ...incident(), events: resolved.events.slice(0, 1) });
    const attempt = (id: string, state: string, opened: string) => ({
      id, server_id: "s", hostname: "web-1", alert_incident_id: ID, metric: "m", instance: "", state,
      opened_at: opened, closed_at: state === "escalated" ? opened : null, max_commands: 3, max_duration_s: 3600, event_count: 4,
    });
    const escalated = { status: 200, body: { incidents: [attempt("r1", "escalated", "2026-10-19T11:00:20Z")], next_cursor: null } };
    const status = (fields: object) => ({
      status: 200,
      body: { alert_incident_id: ID, attempts: 1, latest_id: "r1", latest_state: "escalated", can_run: true, blocked: null, reason: null, refusal: null, ...fields },
    });

    it("offers it on an open alert whose last attempt ended, and starts one", async () => {
      const started = status({ attempts: 2, latest_id: "r2", latest_state: "open", can_run: false, blocked: "in_progress" });
      const after = { status: 200, body: { incidents: [attempt("r2", "open", "2026-10-19T11:59:00Z"), attempt("r1", "escalated", "2026-10-19T11:00:20Z")], next_cursor: null } };
      const fetchMock = stubIncident(open(), escalated, [status({}), started], {
        [RUN]: { status: 201, body: { id: "r2", alert_incident_id: ID, attempt: 2, previous_id: "r1", state: "open" } },
      });
      fetchMock.mockImplementation(
        ((original) => async (input, init) => {
          // The list once the new attempt exists.
          if (String(input) === REMEDIATION.slice(4) && fetchMock.mock.calls.some(([, i]) => i?.method === "POST")) {
            return new Response(JSON.stringify(after.body), { status: 200 });
          }
          return original(input, init);
        })(fetchMock.getMockImplementation()!),
      );
      render(<IncidentPage id={ID} />);
      const button = await screen.findByRole("button", { name: "Run AI again" });
      expect(button).toBeEnabled();
      await userEvent.click(button);
      expect(fetchMock.mock.calls.some(([input, init]) => `${init?.method} ${String(input)}` === RUN)).toBe(true);
      // The new attempt shows, numbered, with the earlier one under it, and no button while it runs.
      const line = (await screen.findByText(/^Attempt 2 · /)).parentElement!;
      expect(line).toHaveTextContent(/AI remediation\s*Open\s*Attempt 2 · 4 events/);
      const earlier = screen.getByRole("list", { name: "Earlier attempts" });
      expect(within(earlier).getByRole("link")).toHaveTextContent(/^Attempt 1: escalated, opened /);
      expect(within(earlier).getByRole("link")).toHaveAttribute("href", expect.stringMatching(/^\/remediation\/audit\/?\?incident=r1$/));
      expect(screen.queryByRole("button", { name: "Run AI again" })).not.toBeInTheDocument();
    });

    it("says why it cannot run before anyone clicks", async () => {
      stubIncident(open(), escalated, status({ can_run: false, blocked: "commands_refused", refusal: "commands_off", reason: "AI commands are off" }));
      render(<IncidentPage id={ID} />);
      expect(await screen.findByRole("button", { name: "Run AI again" })).toBeDisabled();
      expect(
        screen.getByText("Not now: AI commands are off on this server. The Commands tab of its page turns them on, or gives the line to run there."),
      ).toBeInTheDocument();
    });

    it("says AI is off", async () => {
      stubIncident(open(), escalated, status({ can_run: false, blocked: "ai_off" }));
      render(<IncidentPage id={ID} />);
      expect(await screen.findByRole("button", { name: "Run AI again" })).toBeDisabled();
      expect(screen.getByText(/^AI is off\./)).toBeInTheDocument();
    });

    it("offers nothing while an attempt runs, to someone who may not, or once resolved", async () => {
      for (const [detail, run] of [
        [open(), status({ can_run: false, blocked: "in_progress" })],
        [open(), status({ can_run: false, blocked: "role" })],
        [resolved, status({})],
      ] as const) {
        stubIncident(detail, escalated, run);
        const { unmount } = render(<IncidentPage id={ID} />);
        await screen.findByText("AI remediation");
        expect(screen.queryByRole("button", { name: /Run AI/ })).not.toBeInTheDocument();
        unmount();
      }
    });

    it("offers the first run on an open alert that never had one", async () => {
      stubIncident(open(), none, status({ attempts: 0, latest_id: null, latest_state: null }));
      render(<IncidentPage id={ID} />);
      expect(await screen.findByRole("button", { name: "Run AI" })).toBeEnabled();
    });

    it("shows the server's refusal when starting fails", async () => {
      stubIncident(open(), escalated, status({}), {
        [RUN]: { status: 409, body: { error: "an AI remediation of this alert is already running or waiting for approval" } },
      });
      render(<IncidentPage id={ID} />);
      await userEvent.click(await screen.findByRole("button", { name: "Run AI again" }));
      expect(await screen.findByRole("alert")).toHaveTextContent("already running or waiting for approval");
    });

    it("shows the AI's diagnosis where no command could run, with why and the way to change it (ADR-0175)", async () => {
      const diagnosed = {
        ...attempt("r1", "escalated", "2026-10-19T11:00:20Z"),
        event_count: 4,
        diagnosis: "/var grew 20% in an hour; journald writes most of it.",
        diagnosed_at: "2026-10-19T11:00:25Z",
        ai_advice: "Vacuum the journal and cap its size.",
        ai_command: "journalctl --vacuum-size=500M",
        diagnosis_only: "commands_off",
        close_code: "commands_off",
      };
      stubIncident(open(), { status: 200, body: { incidents: [diagnosed], next_cursor: null } }, status({ diagnosis_only: "commands_off", refusal: "commands_off" }));
      render(<IncidentPage id={ID} />);
      const region = await screen.findByRole("region", { name: "AI remediation" });
      expect(within(region).getByText("Diagnosed")).toBeInTheDocument();
      expect(within(region).getByText("AI diagnosis")).toBeInTheDocument();
      expect(within(region).getByText(diagnosed.diagnosis)).toBeInTheDocument();
      expect(region).toHaveTextContent("Next: Vacuum the journal and cap its size.");
      expect(region).toHaveTextContent("Command to run by hand, not run by AI: journalctl --vacuum-size=500M");
      expect(region).toHaveTextContent("nothing ran on the machine");
      expect(region).toHaveTextContent("The AI could not run checks or fixes: AI commands are off on web-1.");
      expect(within(region).getByRole("link", { name: "Turn them on" })).toHaveAttribute(
        "href",
        expect.stringMatching(/^\/servers\/view\/?\?id=.+&tab=commands$/),
      );
      expect(region).toHaveTextContent("The AI will diagnose only: no command can run there.");
      expect(within(region).getByRole("button", { name: "Run AI again" })).toBeEnabled();
    });

    it("says why AI did nothing, with the way out", async () => {
      stubIncident(open(), none, status({ attempts: 0, latest_id: null, latest_state: null, can_run: false, blocked: "ai_off", no_attempt: "ai_off" }));
      const { unmount } = render(<IncidentPage id={ID} />);
      const region = await screen.findByRole("region", { name: "AI remediation" });
      expect(region).toHaveTextContent("AI did not look at this alert: AI is off.");
      expect(within(region).getByRole("link", { name: "Turn on AI" })).toHaveAttribute("href", expect.stringMatching(/^\/settings\/ai\/?$/));
      // Said once, not again beside the button.
      expect(screen.queryByText(/^AI is off\./)).not.toBeInTheDocument();
      unmount();

      stubIncident(open(), none, status({ attempts: 0, latest_id: null, latest_state: null, no_attempt: "too_old" }));
      render(<IncidentPage id={ID} />);
      expect(await screen.findByText(/AI did not look at this alert: it opened more than an hour before AI was on/)).toBeInTheDocument();
      expect(screen.getByRole("button", { name: "Run AI" })).toBeEnabled();
    });

    it("says why an attempt ended, with where to fix it", async () => {
      const failed = {
        ...attempt("r1", "failed", "2026-10-19T11:00:20Z"),
        closed_at: "2026-10-19T11:00:30Z",
        close_code: "ai_failed",
        close_detail: "401 Unauthorized",
        close_reason: "the AI provider failed: 401 Unauthorized",
      };
      stubIncident(open(), { status: 200, body: { incidents: [failed], next_cursor: null } }, status({ latest_state: "failed" }));
      render(<IncidentPage id={ID} />);
      const region = await screen.findByRole("region", { name: "AI remediation" });
      expect(region).toHaveTextContent("The AI provider failed: 401 Unauthorized.");
      expect(within(region).getByRole("link", { name: "Check Settings › AI" })).toHaveAttribute("href", expect.stringMatching(/^\/settings\/ai\/?$/));
    });

    it("does not repeat the AI's words when it handed over", async () => {
      const handed = {
        ...attempt("r1", "escalated", "2026-10-19T11:00:20Z"),
        diagnosis: "Logs fill /var.",
        ai_advice: "Rotate the logs.",
        close_code: "ai_escalated",
        close_detail: "Rotate the logs.",
        close_reason: "the AI escalated: Rotate the logs.",
      };
      stubIncident(open(), { status: 200, body: { incidents: [handed], next_cursor: null } }, status({}));
      render(<IncidentPage id={ID} />);
      const region = await screen.findByRole("region", { name: "AI remediation" });
      expect(region).toHaveTextContent("The AI handed the incident to a person.");
      expect(within(region).getAllByText(/Rotate the logs/)).toHaveLength(1);
      expect(within(region).getAllByRole("link", { name: "See what the AI did" })).toHaveLength(1);
    });

    it("draws the AI's Markdown as bold, code and lists, never as HTML (ADR-0181)", async () => {
      const handed = {
        ...attempt("r1", "escalated", "2026-10-19T11:00:20Z"),
        diagnosis: "The root disk is at **91 %**: 4.6 GB of `.deb` files in `/var/cache/apt/archives`.",
        ai_advice: "- Run `monitorflow-root clean-package-cache`\n- Then <b>check</b> /var/log/*.gz",
        close_code: "ai_escalated",
        close_detail: "x",
        close_reason: "the AI escalated: x",
      };
      stubIncident(open(), { status: 200, body: { incidents: [handed], next_cursor: null } }, status({}));
      render(<IncidentPage id={ID} />);
      const region = await screen.findByRole("region", { name: "AI remediation" });
      expect((await within(region).findByText("91 %")).tagName).toBe("STRONG");
      expect(within(region).getByText("/var/cache/apt/archives").tagName).toBe("CODE");
      expect(within(region).getByText("monitorflow-root clean-package-cache").closest("li")).not.toBeNull();
      expect(region).toHaveTextContent("Next:");
      expect(region).not.toHaveTextContent("**");
      expect(region).toHaveTextContent("Then <b>check</b> /var/log/*.gz");
      expect(region.querySelector("b")).toBeNull();
    });

    it("words it in French", async () => {
      setActiveLocale("fr");
      try {
        stubIncident(open(), escalated, status({}));
        render(<IncidentPage id={ID} />);
        expect(await screen.findByRole("button", { name: "Relancer l’IA" })).toBeEnabled();
      } finally {
        setActiveLocale("en");
      }
    });
  });

  it("refreshes an open incident and stops once it is resolved", async () => {
    const open = { ...resolved, ...incident(), events: resolved.events.slice(0, 1) };
    const fetchMock = stubIncident(open);
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText(/^open · paris/)).toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Summary" })).toHaveTextContent(/Duration1 h/);
    // Only its opening so far, which the tiles, the chart and the header say.
    expect(screen.queryByRole("region", { name: "Timeline" })).toBeNull();
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

describe("IncidentPage acknowledgement", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl(`/incidents/view/?id=${ID}`);
  });
  afterEach(() => vi.useRealTimers());

  const opened: IncidentDetail = { ...incident(), events: resolved.events.slice(0, 1) };

  it("acknowledges, and says who will be told next until then", async () => {
    const waiting: IncidentDetail = {
      ...opened,
      can_acknowledge: true,
      follow_up: {
        state: "waiting",
        stop_reason: null,
        step: 0,
        steps: 2,
        started_at: "2026-10-19T11:58:00Z",
        next_at: "2026-10-19T12:13:00Z",
        next: ["bob", "Slack #ops"],
        policy: "default",
      },
    };
    const acknowledged: IncidentDetail = {
      ...opened,
      acknowledged_by: "alice",
      acknowledged_at: "2026-10-19T12:01:00Z",
      acknowledged_via: "link",
      events: [
        ...opened.events,
        { id: 9, kind: "acknowledged", severity: "critical", value: null, occurred_at: "2026-10-19T12:01:00Z", recorded_at: "2026-10-19T12:01:00Z", details: { username: "alice", via: "link" } },
        { id: 8, kind: "followed_up", severity: "critical", value: null, occurred_at: "2026-10-19T11:59:00Z", recorded_at: "2026-10-19T11:59:00Z", details: { minutes: 15, told: ["bob", "Slack #ops"] } },
      ],
    };
    const fetchMock = stubFetch({
      [`GET /api/incidents/${ID}`]: [
        { status: 200, body: waiting },
        { status: 200, body: acknowledged },
      ],
      [REMEDIATION]: none,
      "POST /api/incidents/acknowledge": { status: 200, body: { acknowledged: [ID] } },
    });
    fetchMock.mockImplementation(
      ((original) => async (input, init) => {
        if (String(input).startsWith("/api/metrics/query_range")) {
          return new Response(JSON.stringify({ query: "…", series: [] }), { status: 200 });
        }
        return original(input, init);
      })(fetchMock.getMockImplementation()!),
    );
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText(/^bob and Slack #ops will be told at .* if nobody acknowledges\.$/)).toBeInTheDocument();
    // The follow-up line says what acknowledging stops: no second explanation.
    expect(screen.queryByText("Says you are on it: nobody else is told next.")).toBeNull();
    await user.click(screen.getByRole("button", { name: "Acknowledge" }));
    // A note, in place and optional (ADR-0166).
    const field = () => screen.getByRole("textbox", { name: "What are you doing about it? (optional)" });
    expect(field()).toHaveFocus();
    expect(field()).not.toBeRequired();
    const confirm = within(screen.getByRole("form", { name: "Acknowledge" })).getByRole("button", { name: "Acknowledge" });
    expect(confirm).toBeEnabled();
    await user.type(field(), "  Replacing the disk ");
    await user.click(confirm);
    expect(await screen.findByText(/from the notification link/)).toBeInTheDocument();
    expect(screen.getByText("alice")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Acknowledge" })).not.toBeInTheDocument();
    expect(screen.getByText("Acknowledged by alice")).toBeInTheDocument();
    expect(screen.getByText("Not acknowledged after 15 minutes: told bob and Slack #ops")).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url]) => url === "/api/incidents/acknowledge");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ incident_ids: [ID], note: "Replacing the disk" });
  });

  it("names no site while there is only one", async () => {
    stubIncident(resolved);
    render(
      <As user={oneSite(viewer)}>
        <IncidentPage id={ID} />
      </As>,
    );
    expect(await screen.findByText(/^was Warning$/)).toBeInTheDocument();
    expect(screen.queryByText(/· paris/)).toBeNull();
  });

  it("says when warnings are not followed up and when everyone was told", async () => {
    stubIncident({ ...opened, severity: "warning" });
    const { unmount } = render(<IncidentPage id={ID} />);
    expect(await screen.findByText("Warnings are not followed up.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Acknowledge" })).not.toBeInTheDocument();
    unmount();
    stubIncident({
      ...opened,
      follow_up: { state: "stopped", stop_reason: "finished", step: 1, steps: 1, started_at: "2026-10-19T11:00:00Z", next_at: null, next: [], policy: "default" },
    });
    render(<IncidentPage id={ID} />);
    expect(await screen.findByText("Everyone in the follow-up was told; nobody acknowledged.")).toBeInTheDocument();
  });
});
