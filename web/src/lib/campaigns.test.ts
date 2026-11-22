import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { WEB1, WEB2, campaign, detail, server } from "@/test/campaigns";

import type { CampaignRequest } from "./api";
import {
  campaignSubline,
  campaignTargets,
  canAct,
  canDelete,
  canEdit,
  copyName,
  describeThreshold,
  describeUpdate,
  describeWindow,
  draftFromCampaign,
  emptyDraft,
  newWindow,
  planWaves,
  searchServers,
  updateSources,
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
    expect(planWaves(ids(2), [{ size: 2 }, { size: 1 }])).toEqual({ error: "batch 2 would be empty" });
    expect(planWaves(ids(2), [{ servers: ["s1"] }, { servers: ["s1"] }])).toEqual({
      error: "a server is in more than one batch",
    });
    expect(planWaves(ids(2), [{ servers: ["x"] }])).toEqual({
      error: "a server of batch 1 is not a target of the campaign",
    });
    expect(planWaves(ids(2), [{ percent: 0 }])).toEqual({ error: "a batch percentage is 1 to 100" });
    expect(planWaves(ids(200), Array(101).fill({ size: 1 }))).toEqual({ error: "at most 100 batches" });
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
    expect(validateCampaign(valid({ packages: ["auditd-"] }), ["s1"])).toEqual(['Invalid package name "auditd-".']);
    expect(validateCampaign(valid({ packages: ["auditd-+"] }), ["s1"])).toEqual(['Invalid package name "auditd-+".']);
    expect(validateCampaign(valid({ packages: ["g++", "libstdc++6:amd64", "a"] }), ["s1"])).toEqual([]);
    expect(validateCampaign(valid({ failure_threshold: { value: 101, unit: "percent" } }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ failure_threshold: { value: 1.5, unit: "count" } }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ command_ttl_s: 30 }), ["s1"])).toHaveLength(1);
    expect(validateCampaign(valid({ waves: [{ size: 1 }, { size: 1 }] }), ["s1"])).toEqual([
      "Batches: batch 2 would be empty.",
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
      windows: [{ ...newWindow("weekly"), weekdays: [7, 6], timezone: " UTC " }],
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

  it("sends a one-time window's local times as ISO times, empty when not given", () => {
    const window = newWindow("once");
    const windows = toRequest({
      ...emptyDraft("c1"),
      windows: [
        { ...window, startsAt: "2026-11-07T22:00", endsAt: "nonsense" },
        { ...window, startsAt: "", endsAt: "" },
      ],
    }).windows;
    expect(windows).toEqual([
      { kind: "once", starts_at: new Date(2026, 10, 7, 22, 0).toISOString(), ends_at: "" },
      { kind: "once", starts_at: "", ends_at: "" },
    ]);
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

  it("leaves out hosts without package updates, unless it updates agents", () => {
    const servers = [server({ id: "a" }), server({ id: "b", package_updates: false })];
    expect(campaignTargets(servers, "c1", "site-1", ["b"]).map((s) => s.id)).toEqual(["a"]);
    expect(campaignTargets(servers, "c1", "site-1", [], true).map((s) => s.id)).toEqual(["a", "b"]);
  });
});

describe("third-party apps (ADR-0138)", () => {
  it("adds the chosen managers, and drops OS options without the OS", () => {
    const both = toRequest({ ...emptyDraft("c1"), sources: "both", managers: ["flatpak"], update: "security" });
    expect(both).toMatchObject({ security_only: true, third_party: [{ manager: "flatpak", packages: [] }] });
    expect("include_os" in both).toBe(false);
    const apps = toRequest({ ...emptyDraft("c1"), sources: "apps", update: "packages", packages: "openssl" });
    expect(apps).toMatchObject({
      packages: [],
      security_only: false,
      include_os: false,
      third_party: [
        { manager: "winget", packages: [] },
        { manager: "flatpak", packages: [] },
        { manager: "snap", packages: [] },
      ],
    });
    const os = toRequest(emptyDraft("c1"));
    expect("third_party" in os || "include_os" in os).toBe(false);
    const agent = toRequest({ ...emptyDraft("c1"), sources: "apps", update: "agent", agentVersion: "0.5.0" });
    expect("third_party" in agent || "include_os" in agent).toBe(false);
    expect(validateCampaign(valid({ third_party: [] }), ["s1"])).toEqual(["Choose at least one app manager."]);
  });

  it("targets, without the OS, the servers a chosen manager runs on", () => {
    const servers = [
      server({ id: "w", hostname: "win-1", os_family: "windows" }),
      server({ id: "l", hostname: "web-1", package_updates: false }),
    ];
    expect(campaignTargets(servers, "c1", "site-1", [], false, ["flatpak"]).map((s) => s.id)).toEqual(["l"]);
    expect(campaignTargets(servers, "c1", "site-1", [], false, ["winget"]).map((s) => s.id)).toEqual(["w"]);
    expect(campaignTargets(servers, "c1", "site-1", []).map((s) => s.id)).toEqual(["w"]);
  });

  it("names the sources of a campaign", () => {
    const spec = { packages: [], security_only: false, allow_reboot: false };
    expect(describeUpdate({ ...spec, include_os: false, third_party: [{ manager: "winget", packages: [] }] })).toBe(
      "Third-party apps: winget",
    );
    expect(describeUpdate({ ...spec, security_only: true, third_party: [{ manager: "snap", packages: [] }] })).toBe(
      "Security updates, and third-party apps: Snap",
    );
    expect(campaignSubline(campaign({ spec: { ...spec, include_os: false, third_party: [{ manager: "flatpak", packages: [] }] } }))).toBe(
      "2 batches · apps: Flatpak",
    );
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
    expect(describeThreshold({ value: 10, unit: "percent" })).toBe("more than 10% of a batch fails");
    expect(
      describeWindow({ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "Europe/Paris" }),
    ).toBe("Sat, Sun 22:00–04:00 (Europe/Paris)");
  });

  it("sums a campaign up on one line and windows in a cell", () => {
    expect(campaignSubline(campaign())).toBe("2 batches · security only");
    expect(
      campaignSubline(
        campaign({
          wave_count: 1,
          spec: { packages: [], security_only: false, allow_reboot: true },
          sources: { mode: "replace", mirrors: [{ name: "debian-12", snapshot: "s1" }] },
        }),
      ),
    ).toBe("1 batch · snapshot debian-12/s1 · all packages · reboot allowed");
    expect(shortWindow({ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "UTC" })).toBe(
      "Sat, Sun 22:00–04:00",
    );
    const daily = { kind: "weekly" as const, weekdays: [1, 2, 3, 4, 5, 6, 7], start: "04:00", end: "05:00", timezone: "UTC" };
    expect(shortWindow(daily)).toBe("Every day 04:00–05:00");
    expect(describeWindow(daily)).toBe("Every day 04:00–05:00 (UTC)");
    const once = (a: Date, b: Date) => shortWindow({ kind: "once", starts_at: a.toISOString(), ends_at: b.toISOString() });
    expect(once(new Date(2026, 9, 23, 2, 0), new Date(2026, 9, 23, 5, 0))).toBe("23 Oct 02:00–05:00");
    expect(once(new Date(2026, 9, 23, 22, 0), new Date(2026, 9, 24, 4, 0))).toBe("23 Oct 22:00–24 Oct 04:00");
  });
});

describe("agent update campaigns", () => {
  it("send the version only, and say so", () => {
    const request = toRequest({ ...emptyDraft("c1"), name: "a", update: "agent", agentVersion: "0.3.0", allowReboot: true });
    expect(request).toMatchObject({ packages: [], security_only: false, allow_reboot: false, agent_version: "0.3.0" });
    expect(toRequest(emptyDraft("c1")).agent_version).toBeUndefined();
    expect(validateCampaign({ ...request, agent_version: "" }, ["s1"])).toContain("Choose the agent version to update to.");
    expect(describeUpdate({ packages: [], security_only: false, allow_reboot: false }, "0.3.0")).toBe("Agent update to 0.3.0");
    expect(
      campaignSubline({ wave_count: 2, sources: null, spec: { packages: [], security_only: false, allow_reboot: false }, agent_version: "0.3.0" }),
    ).toBe("2 batches · agent 0.3.0");
  });
});

describe("in French", () => {
  it("describes campaigns and their mistakes", () => {
    setActiveLocale("fr");
    expect(describeThreshold({ value: 1, unit: "count" })).toBe("plus de 1 serveur d’un lot en échec");
    expect(describeThreshold({ value: 10, unit: "percent" })).toBe("plus de 10\u00a0% d’un lot en échec");
    expect(describeUpdate({ packages: [], security_only: true, allow_reboot: true })).toBe(
      "Mises à jour de sécurité, redémarrage autorisé",
    );
    expect(shortWindow({ kind: "weekly", weekdays: [1, 6], start: "22:00", end: "04:00", timezone: "UTC" })).toBe(
      "lun., sam. 22:00–04:00",
    );
    expect(validateCampaign(valid({ server_ids: [] }), [])).toEqual(["Choisissez un site ou des serveurs à mettre à jour."]);
    expect(validateCampaign(valid({ waves: [{ size: 1 }, { size: 1 }] }), ["s1"])).toEqual([
      "Lots\u00a0: le lot 2 serait vide.",
    ]);
  });
});

describe("editing and copying (ADR-0166)", () => {
  it("edits what queued nothing and deletes drafts and finished campaigns", () => {
    const idle = { ...detail().progress, in_progress: 0 };
    expect(canEdit(campaign({ state: "draft", current_wave: null }))).toBe(true);
    expect(canEdit(campaign({ state: "scheduled", current_wave: null }))).toBe(true);
    expect(canEdit(campaign({ state: "paused", current_wave: 0 }))).toBe(false);
    expect(canEdit(campaign({ state: "stopped", current_wave: null }))).toBe(false);
    expect(canDelete(campaign({ state: "draft" }))).toBe(true);
    expect(canDelete(campaign({ state: "completed", progress: idle }))).toBe(true);
    expect(canDelete(campaign({ state: "stopped" }))).toBe(false);
    expect(canDelete(campaign({ state: "running", progress: idle }))).toBe(false);
  });

  it("fills the form from a campaign, the request when kept, else its batches", () => {
    const pin = { mode: "add" as const, mirrors: [{ name: "debian-12", snapshot: "s1" }] };
    const kept = draftFromCampaign(
      detail({ site_id: "site-1", server_ids: [WEB2], wave_specs: [{ servers: [WEB1] }], sources: pin }),
    );
    expect(kept).toMatchObject({ siteId: "site-1", serverIds: [WEB2], update: "security", sources: "os", pin });
    expect(kept.waves).toEqual([{ kind: "servers", value: "1", servers: [WEB1] }]);
    expect(toRequest(kept)).toMatchObject({ waves: [{ servers: [WEB1] }], security_only: true, sources: pin });
    // Before ADR-0166: servers outside the site are named, batches become sizes.
    const older = draftFromCampaign(detail({ site_id: null }));
    expect(older.serverIds).toEqual([WEB1, WEB2]);
    expect(older.waves).toEqual([{ kind: "size", value: "1", servers: [] }]);
    expect(older.windows[0]).toMatchObject({ kind: "weekly", weekdays: [6, 7], timezone: "Europe/Paris" });
    const appsOnly = { packages: [], security_only: false, allow_reboot: true, include_os: false };
    const apps = draftFromCampaign(detail({ spec: { ...appsOnly, third_party: [{ manager: "snap", packages: [] }] } }));
    expect(apps).toMatchObject({ sources: "apps", managers: ["snap"], update: "all", allowReboot: true });
  });

  it("names a copy within the longest name", () => {
    expect(copyName("October")).toBe("Copy of October");
    expect([...copyName("x".repeat(64))]).toHaveLength(64);
  });
});

describe("searchServers", () => {
  it("matches every word in the name, OS or site, in any case", () => {
    const servers = [
      server({ id: "a", hostname: "web-1" }),
      server({ id: "b", hostname: "db-1", os_family: "rhel", os_version: "rocky 9.4", site_name: "lyon" }),
    ];
    const ids = (query: string) => searchServers(servers, query).map((s) => s.id);
    expect(ids("")).toEqual(["a", "b"]);
    expect(ids("WEB")).toEqual(["a"]);
    expect(ids("rocky")).toEqual(["b"]);
    expect(ids("debian 12")).toEqual(["a"]);
    expect(ids("lyon db")).toEqual(["b"]);
    expect(ids("lyon web")).toEqual([]);
  });
});

describe("update sources", () => {
  it("come from two independent choices, and need one", () => {
    expect([updateSources(true, false), updateSources(true, true), updateSources(false, true)]).toEqual(["os", "both", "apps"]);
    const none = toRequest({ ...emptyDraft("c1"), sources: updateSources(false, false) });
    expect(none).toMatchObject({ include_os: false });
    expect(none.third_party).toBeUndefined();
    expect(validateCampaign(none, ["s1"])).toContain("Choose operating system updates, third-party apps or both.");
  });
});

describe("monthly and yearly windows (ADR-0167)", () => {
  const monthly = { start: "22:00", end: "02:00", timezone: "Europe/Paris" };
  it("describe the day they open on", () => {
    const all = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    expect(describeWindow({ kind: "monthly", month_week: 2, weekday: 2, months: all, ...monthly })).toBe(
      "Second Tuesday of each month 22:00–02:00 (Europe/Paris)",
    );
    expect(shortWindow({ kind: "monthly", month_week: 5, weekday: 5, months: [3, 6], ...monthly })).toBe(
      "Last Friday of Mar, Jun 22:00–02:00",
    );
    expect(shortWindow({ kind: "monthly", month_day: 15, month_week: null, weekday: null, months: all, ...monthly })).toBe(
      "Day 15 of each month 22:00–02:00",
    );
    expect(shortWindow({ kind: "monthly", month_day: 31, months: [12], ...monthly })).toBe("Every year on December 31 22:00–02:00");
    expect(shortWindow({ kind: "monthly", month_week: 1, weekday: 1, months: [9], ...monthly })).toBe(
      "First Monday of September 22:00–02:00",
    );
    setActiveLocale("fr");
    try {
      expect(shortWindow({ kind: "monthly", month_week: 2, weekday: 2, months: all, ...monthly })).toBe(
        "Deuxième mardi de chaque mois 22:00–02:00",
      );
      expect(shortWindow({ kind: "monthly", month_day: 1, months: [1], ...monthly })).toBe("Chaque année le 1er janvier 22:00–02:00");
    } finally {
      setActiveLocale("en");
    }
  });

  it("go to the API and back, and are checked", () => {
    const yearly = { ...newWindow("yearly"), months: [12], monthDay: "31" };
    const weekday = { ...newWindow("monthly"), monthMode: "weekday" as const, monthWeek: 5, weekday: 5, months: [3, 1] };
    const request = toRequest({ ...emptyDraft("c1"), windows: [yearly, weekday] });
    expect(request.windows).toEqual([
      { kind: "monthly", month_day: 31, months: [12], start: "22:00", end: "04:00", timezone: expect.any(String) },
      { kind: "monthly", month_week: 5, weekday: 5, months: [1, 3], start: "22:00", end: "04:00", timezone: expect.any(String) },
    ]);
    const back = draftFromCampaign(detail({ windows: request.windows }));
    expect(back.windows[0]).toMatchObject({ kind: "yearly", monthMode: "day", monthDay: "31", months: [12] });
    expect(back.windows[1]).toMatchObject({ kind: "monthly", monthMode: "weekday", monthWeek: 5, weekday: 5 });
    const bad = toRequest({ ...emptyDraft("c1"), windows: [{ ...newWindow("monthly"), monthDay: "0", months: [] }] });
    expect(validateCampaign(bad, ["s1"])).toEqual(
      expect.arrayContaining(["Window 1: the day of the month is 1 to 31.", "Window 1: choose at least one month."]),
    );
  });
});
