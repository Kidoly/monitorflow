import { describe, expect, it } from "vitest";

import { NO_FILTERS, apiQuery, filtersFromQuery, filtersToQuery, incidentSeconds } from "./incidents";
import { incidentsHref } from "./routes";

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
