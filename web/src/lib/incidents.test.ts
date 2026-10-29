import { describe, expect, it } from "vitest";

import {
  INCIDENT_MARGINS,
  NO_FILTERS,
  apiQuery,
  filtersFromQuery,
  filtersToQuery,
  incidentSeconds,
  incidentTitle,
  incidentWindow,
  isOffline,
  resolvesWhen,
} from "./incidents";
import { incidentPeerHref, incidentsHref } from "./routes";

const SITE = "0b8d2f6e-1c4a-4f7e-9d2b-3a5c6e7f8a90";
const SERVER = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";

describe("incident filters", () => {
  it("round-trip through the query string", () => {
    const filters = { state: "open", severity: "critical", siteId: SITE, serverId: SERVER } as const;
    const query = filtersToQuery(filters);
    expect(query).toBe(`state=open&severity=critical&site=${SITE}&server=${SERVER}`);
    expect(filtersFromQuery(new URLSearchParams(query))).toEqual(filters);
  });

  it("leave defaults out of the query string", () => {
    expect(filtersToQuery(NO_FILTERS)).toBe("");
    expect(filtersToQuery({ ...NO_FILTERS, state: "resolved" })).toBe("state=resolved");
  });

  it("ignore unknown and malformed values", () => {
    const params = new URLSearchParams("state=closed&severity=Critical&site=paris&server=1&sort=asc");
    expect(filtersFromQuery(params)).toEqual(NO_FILTERS);
    expect(filtersFromQuery(new URLSearchParams(`server=${SERVER.toUpperCase()}`)).serverId).toBe(SERVER);
  });

  it("map to the API's parameters", () => {
    expect(apiQuery(NO_FILTERS)).toEqual({});
    expect(apiQuery({ state: "resolved", severity: "warning", siteId: SITE, serverId: SERVER })).toEqual({
      state: "resolved",
      severity: "warning",
      site_id: SITE,
      server_id: SERVER,
    });
  });

  it("build links to the list", () => {
    expect(incidentsHref()).toBe("/incidents/");
    expect(incidentsHref({ state: "open", serverId: SERVER })).toBe(`/incidents/?state=open&server=${SERVER}`);
  });
});

describe("incidentSeconds", () => {
  const now = new Date("2026-10-19T12:00:00Z");

  it("measures resolved incidents to their resolution and open ones to now", () => {
    expect(incidentSeconds("2026-10-19T11:00:00Z", "2026-10-19T11:30:00Z", now)).toBe(1800);
    expect(incidentSeconds("2026-10-19T11:00:00Z", null, now)).toBe(3600);
  });

  it("never goes negative when the agent's clock is ahead", () => {
    expect(incidentSeconds("2026-10-19T12:05:00Z", null, now)).toBe(0);
  });
});

describe("incidentWindow", () => {
  const now = new Date("2026-10-19T12:00:00Z");
  const at = (iso: string) => Date.parse(iso) / 1000;

  it("frames an open incident from a margin before it to now", () => {
    expect(incidentWindow("2026-10-19T11:50:00Z", null, now)).toEqual({
      start: at("2026-10-19T11:35:00Z"),
      end: at("2026-10-19T12:00:00Z"),
      step: 30,
    });
  });

  it("frames a resolved incident with a margin on both sides", () => {
    // 4 h long: a 1 h margin on each side, 6 h at 2 min steps.
    expect(incidentWindow("2026-10-18T10:00:00Z", "2026-10-18T14:00:00Z", now)).toEqual({
      start: at("2026-10-18T09:00:00Z"),
      end: at("2026-10-18T15:00:00Z"),
      step: 120,
    });
  });

  it("keeps long incidents to a few hundred points", () => {
    const { start, end, step } = incidentWindow("2026-09-19T12:00:00Z", null, now);
    expect((end - start) / step).toBeLessThanOrEqual(360);
    expect(start % step).toBe(0);
  });
});

describe("incident window margins and recovery", () => {
  const now = new Date("2026-10-19T12:00:00Z");

  it("uses the margin chosen with the window buttons", () => {
    const window = incidentWindow("2026-10-19T11:00:00Z", "2026-10-19T11:10:00Z", now, 3 * 3600);
    expect(window.start).toBeLessThanOrEqual(Date.parse("2026-10-19T08:00:00Z") / 1000);
    expect(window.end).toBeGreaterThanOrEqual(now.getTime() / 1000 - window.step);
    expect(INCIDENT_MARGINS.map((m) => m.label)).toEqual(["± 3 h", "± 12 h", "± 3 d"]);
  });

  it("says when the engine resolves, past the lowest threshold by the hysteresis", () => {
    const pct = (v: number) => `${Math.round(v * 100)}%`;
    const threshold = { metric: "m", instance: null, comparison: ">" as const, warning: 0.85, critical: 0.95, duration_s: 300, hysteresis: 0.02 };
    expect(resolvesWhen(threshold, pct)).toBe("the value is at or below 83%");
    expect(resolvesWhen({ ...threshold, warning: null }, pct)).toBe("the value is at or below 93%");
    expect(resolvesWhen({ ...threshold, comparison: "<", warning: 0.1, critical: null }, pct)).toBe("the value is at or above 12%");
    expect(resolvesWhen({ ...threshold, warning: null, critical: null }, pct)).toBeNull();
  });
});

describe("offline incidents", () => {
  it("are titled and linked to their server or relay", () => {
    expect(isOffline({ metric: "offline" })).toBe(true);
    expect(incidentTitle({ metric: "offline", instance: "" })).toBe("Offline");
    expect(incidentTitle({ metric: "monitorflow_disk_used_ratio", instance: "/var" })).toBe("Disk used /var");
    expect(incidentPeerHref({ server_id: "s1" })).toBe("/servers/view/?id=s1");
    expect(incidentPeerHref({ server_id: null, relay_id: "r1" })).toBe("/admin/relays/view/?id=r1");
  });
});
