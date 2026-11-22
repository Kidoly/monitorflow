import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { ApiError, DEMO_REFUSAL, api, describeError } from "../api";

import { SERVERS } from "./fleet";

describe("static demo", () => {
  beforeEach(() => {
    vi.stubEnv("NEXT_PUBLIC_MONITORFLOW_DEMO", "1");
  });
  afterEach(() => {
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });

  it("answers reads from the made-up data, never the network", async () => {
    const fetchMock = vi.fn<typeof fetch>();
    vi.stubGlobal("fetch", fetchMock);
    const me = await api.me();
    expect(me.username).toBe("demo");
    const { servers } = await api.servers();
    expect(servers.length).toBe(SERVERS.length);
    expect(servers.map((s) => s.hostname)).toContain("db-02");
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("keeps endpoints consistent: the open incidents match the fleet's states", async () => {
    const { incidents } = await api.incidents({ state: "open" });
    const summary = await api.serverSummary({ customerId: null, siteId: null });
    expect(incidents.length).toBe(summary.critical + summary.warning + summary.acknowledged + summary.offline);
    const { approvals } = await api.approvals();
    expect(approvals[0]?.hostname).toBe("db-02");
    expect(incidents.some((i) => i.id === approvals[0]?.alert_incident_id)).toBe(true);
  });

  it("draws curves relative to now", async () => {
    const end = new Date();
    const start = new Date(end.getTime() - 3600_000);
    const { series } = await api.queryRange({ serverId: SERVERS[0]!.id, metric: "monitorflow_cpu_usage_ratio", start, end, step: 60 });
    const values = series[0]!.points.map(([, v]) => v);
    expect(values.length).toBeGreaterThan(50);
    expect(values.every((v) => v !== null && v > 0 && v < 1)).toBe(true);
  });

  it("refuses every write with the demo message, in the reader's words", async () => {
    const refusals = await Promise.allSettled([
      api.approve("x"),
      api.createSilence({ scope: "all", ends_at: new Date().toISOString(), reason: "test" }),
      api.removeUser("x"),
      api.setMirrorMode("x", "full"),
      api.logout(),
    ]);
    for (const result of refusals) {
      expect(result.status).toBe("rejected");
      const error = (result as PromiseRejectedResult).reason as unknown;
      expect(error).toBeInstanceOf(ApiError);
      expect((error as ApiError).message).toBe(DEMO_REFUSAL);
      expect(describeError(error)).toBe("This is a demo: changes are not saved.");
    }
  });

  it("answers an unknown read with a 404 rather than a wrong shape", async () => {
    await expect(api.server("missing")).rejects.toMatchObject({ status: 404 });
  });
});

describe("without the demo flag", () => {
  it("leaves fetch to the API untouched", async () => {
    vi.stubEnv("NEXT_PUBLIC_MONITORFLOW_DEMO", "");
    const fetchMock = vi.fn<typeof fetch>(async () => new Response(JSON.stringify({ id: "u1", username: "alice", roles: [] }), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    const me = await api.me();
    expect(me.username).toBe("alice");
    expect(fetchMock).toHaveBeenCalledOnce();
    expect(fetchMock.mock.calls[0]![0]).toBe("/api/auth/me");
    vi.unstubAllEnvs();
    vi.unstubAllGlobals();
  });
});

describe("static demo uptime", () => {
  beforeEach(() => {
    vi.stubEnv("NEXT_PUBLIC_MONITORFLOW_DEMO", "1");
  });
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("shows a few outages on the uptime page, each in its windows and bars", async () => {
    const { rows } = await api.uptime({ time_zone: "UTC" });
    const app = rows.find((r) => r.name === "app-04")!;
    expect(app.outages).toBe(1);
    expect(app.days.at(-1)!.down_seconds).toBe(2_520);
    expect(app.windows.day).toBeLessThan(99);
    expect(app.windows.quarter).toBeGreaterThan(99.9);
    const web = rows.find((r) => r.name === "web-01")!;
    expect(web.windows).toEqual({ day: 100, week: 100, month: 100, quarter: 100 });
    expect(web.days.every((d) => d.down_seconds === 0)).toBe(true);
  });
});
