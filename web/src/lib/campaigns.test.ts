import { describe, expect, it } from "vitest";

import { campaign, server } from "@/test/campaigns";

import type { CampaignRequest } from "./api";
import {
  campaignSubline,
  campaignTargets,
  canAct,
  describeThreshold,
  describeUpdate,
  describeWindow,
  emptyDraft,
  planWaves,
  shortWindow,
  toRequest,
  validateCampaign,
} from "./campaigns";

const ids = (n: number) => Array.from({ length: n }, (_, i) => `s${i + 1}`);

const valid = (overrides: Partial<CampaignRequest> = {}): CampaignRequest => ({
  ...toRequest({ ...emptyDraft("c1"), name: "October", serverIds: ["s1"] }),
  ...overrides,
});

describe("planWaves", () => {
  it("places explicit servers first, then sizes and percentages, the rest last", () => {
    const plan = planWaves(ids(10), [{ servers: ["s10"] }, { size: 2 }, { percent: 25 }]);
    expect(plan).toEqual({ waves: [["s10"], ["s1", "s2"], ["s3", "s4", "s5"], ["s6", "s7", "s8", "s9"]] });
    expect(planWaves(ids(3), [])).toEqual({ waves: [ids(3)] });
  });

  it("refuses what the server refuses", () => {
    expect(planWaves(ids(2), [{ size: 2 }, { size: 1 }])).toEqual({ error: "wave 2 would be empty" });
    expect(planWaves(ids(2), [{ servers: ["s1"] }, { servers: ["s1"] }])).toEqual({
      error: "a server is in more than one wave",
    });
    expect(planWaves(ids(2), [{ servers: ["x"] }])).toEqual({
      error: "a server of wave 1 is not a target of the campaign",
    });
    expect(planWaves(ids(2), [{ percent: 0 }])).toEqual({ error: "a wave percentage is 1 to 100" });
    expect(planWaves(ids(200), Array(101).fill({ size: 1 }))).toEqual({ error: "at most 100 waves" });
  });
});

describe("validateCampaign", () => {
  it("accepts a minimal campaign", () => {
    expect(validateCampaign(valid(), ["s1"])).toEqual([]);
  });

  it("checks the name, targets, packages and threshold like the API", () => {
    expect(validateCampaign(valid({ name: " October" }), ["s1"])[0]).toMatch(/name must be 1 to 64/);
    expect(validateCampaign(valid({ name: "x".repeat(65) }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ server_ids: [] }), [])).toEqual(["Choose a site or servers to update."]);
    expect(validateCampaign(valid({ site_id: "site-1", server_ids: [] }), [])).toEqual([
      "The campaign targets no server.",
    ]);
    expect(validateCampaign(valid({ packages: ["openssl", "-rf"] }), ["s1"])).toEqual(['Invalid package name "-rf".']);
    expect(validateCampaign(valid({ failure_threshold: { value: 101, unit: "percent" } }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ failure_threshold: { value: 1.5, unit: "count" } }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ command_ttl_s: 30 }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ waves: [{ size: 1 }, { size: 1 }] }), ["s1"])).toEqual([
      "Waves: wave 2 would be empty.",
    ]);
  });

  it("checks windows", () => {
    const weekly = { kind: "weekly" as const, weekdays: [6], start: "22:00", end: "04:00", timezone: "Europe/Paris" };
    expect(validateCampaign(valid({ windows: [weekly] }), ["s1"])).toEqual([]);
    // Midnight as an end is the end of the day.
    expect(validateCampaign(valid({ windows: [{ ...weekly, start: "00:00", end: "00:00" }] }), ["s1"])).toEqual([]);
    expect(validateCampaign(valid({ windows: [{ ...weekly, end: "22:00" }] }), ["s1"])).toEqual([
      "Window 1: it must end at another time than it starts.",
    ]);
    expect(validateCampaign(valid({ windows: [{ ...weekly, weekdays: [], timezone: "" }] }), ["s1"])).toHaveLength(2);
    const once = { kind: "once" as const, starts_at: "2026-11-02T04:00:00Z", ends_at: "2026-11-01T20:00:00Z" };
    expect(validateCampaign(valid({ windows: [once] }), ["s1"])).toEqual(["Window 1: it must end after it starts."]);
    expect(validateCampaign(valid({ windows: [{ ...once, starts_at: "" }] }), ["s1"])).toEqual([
      "Window 1: give a start and an end.",
    ]);
  });
});

describe("toRequest", () => {
  it("builds the API body from the form", () => {
    const request = toRequest({
      ...emptyDraft("c1"),
      name: "October",
      siteId: "site-1",
      waves: [
        { kind: "size", value: "1", servers: [] },
        { kind: "percent", value: "50", servers: [] },
        { kind: "servers", value: "", servers: ["s9"] },
      ],
      update: "packages",
      packages: "openssl,  libssl3\ncurl",
      thresholdValue: "10",
      thresholdUnit: "percent",
      windows: [{ kind: "weekly", weekdays: [7, 6], start: "22:00", end: "04:00", timezone: " UTC ", startsAt: "", endsAt: "" }],
    });
    expect(request).toEqual({
      customer_id: "c1",
      name: "October",
      site_id: "site-1",
      server_ids: [],
      waves: [{ size: 1 }, { percent: 50 }, { servers: ["s9"] }],
      packages: ["openssl", "libssl3", "curl"],
      security_only: false,
      allow_reboot: false,
      command_ttl_s: 3600,
      failure_threshold: { value: 10, unit: "percent" },
      windows: [{ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "UTC" }],
    });
    expect(toRequest({ ...emptyDraft("c1"), update: "security", packages: "ignored" })).toMatchObject({
      packages: [],
      security_only: true,
    });
    expect("site_id" in toRequest(emptyDraft("c1"))).toBe(false);
  });

  it("targets the customer's unrevoked servers of the site or the list, by hostname", () => {
    const servers = [
      server({ id: "b", hostname: "web-2" }),
      server({ id: "a", hostname: "web-1" }),
      server({ id: "r", hostname: "web-0", revoked_at: "2026-10-20T10:00:00Z" }),
      server({ id: "o", hostname: "db-1", site_id: "site-2" }),
      server({ id: "x", hostname: "aaa", customer_id: "c2" }),
    ];
    expect(campaignTargets(servers, "c1", "site-1", []).map((s) => s.id)).toEqual(["a", "b"]);
    expect(campaignTargets(servers, "c1", "site-1", ["o", "x"]).map((s) => s.id)).toEqual(["o", "a", "b"]);
    expect(campaignTargets(servers, "c1", "", ["b"]).map((s) => s.id)).toEqual(["b"]);
  });
});

describe("campaign states and labels", () => {
  it("follows the API's state machine", () => {
    expect(canAct("start", "draft")).toBe(true);
    expect(canAct("start", "scheduled")).toBe(false);
    expect(canAct("pause", "running")).toBe(true);
    expect(canAct("resume", "paused")).toBe(true);
    expect(canAct("resume", "running")).toBe(false);
    for (const state of ["stopped", "completed", "failed"] as const) {
      expect(canAct("stop", state)).toBe(false);
    }
  });

  it("describes updates, thresholds and windows", () => {
    expect(describeUpdate({ packages: [], security_only: true, allow_reboot: true })).toBe(
      "Security updates, reboot allowed",
    );
    expect(describeUpdate({ packages: ["curl"], security_only: false, allow_reboot: false })).toBe("Packages: curl");
    expect(describeThreshold({ value: 0, unit: "count" })).toBe("first failure");
    expect(describeThreshold({ value: 10, unit: "percent" })).toBe("more than 10% of a wave fails");
    expect(
      describeWindow({ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "Europe/Paris" }),
    ).toBe("Sat, Sun 22:00–04:00 (Europe/Paris)");
  });

  it("sums a campaign up on one line and windows in a cell", () => {
    expect(campaignSubline(campaign())).toBe("2 waves · security only");
    expect(
      campaignSubline(
        campaign({
          wave_count: 1,
          spec: { packages: [], security_only: false, allow_reboot: true },
          sources: { mode: "replace", mirrors: [{ name: "debian-12", snapshot: "s1" }] },
        }),
      ),
    ).toBe("1 wave · snapshot debian-12/s1 · all packages · reboot allowed");
    expect(shortWindow({ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "UTC" })).toBe(
      "Sat, Sun 22:00–04:00",
    );
    const once = (a: Date, b: Date) => shortWindow({ kind: "once", starts_at: a.toISOString(), ends_at: b.toISOString() });
    expect(once(new Date(2026, 9, 23, 2, 0), new Date(2026, 9, 23, 5, 0))).toBe("23 Oct 02:00–05:00");
    expect(once(new Date(2026, 9, 23, 22, 0), new Date(2026, 9, 24, 4, 0))).toBe("23 Oct 22:00–24 Oct 04:00");
  });
});
