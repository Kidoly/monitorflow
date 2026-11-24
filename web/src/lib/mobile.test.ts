import { describe, expect, it } from "vitest";

import type { OnCallSchedule, OnCallTurn } from "./api";
import { base64UrlToBytes, mobileIncidentHref, onCallNow, phoneName, pushSupport, silenceFor } from "./mobile";

const IPHONE = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_4 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Mobile/15E148 Safari/604.1";
const PIXEL = "Mozilla/5.0 (Linux; Android 14; Pixel 8) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Mobile Safari/537.36";
const REDUCED = "Mozilla/5.0 (Linux; Android 10; K) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Mobile Safari/537.36";

describe("phones", () => {
  it("names a phone from its browser", () => {
    expect(phoneName(IPHONE)).toBe("iPhone");
    expect(phoneName(PIXEL)).toBe("Pixel 8");
    // Chrome hides the model now: "K" says nothing.
    expect(phoneName(REDUCED)).toBe("Android");
    expect(phoneName("Mozilla/5.0 (X11; Linux x86_64)")).toBe("Phone");
  });

  it("says where push stands", () => {
    const env = { userAgent: PIXEL, standalone: false, hasPushManager: true, hasServiceWorker: true, permission: "default" as const };
    expect(pushSupport(env)).toBe("ready");
    expect(pushSupport({ ...env, permission: "denied" })).toBe("denied");
    expect(pushSupport({ ...env, hasPushManager: false })).toBe("unsupported");
    expect(pushSupport({ ...env, permission: null })).toBe("unsupported");
    // Safari pushes only to the app on the home screen.
    expect(pushSupport({ ...env, userAgent: IPHONE })).toBe("install");
    expect(pushSupport({ ...env, userAgent: IPHONE, standalone: true })).toBe("ready");
  });

  it("reads the server's key", () => {
    expect(Array.from(base64UrlToBytes("AP-_"))).toEqual([0, 255, 191]);
    expect(base64UrlToBytes("BA").length).toBe(1);
  });

  it("links to an incident, with its silence choices from a notification", () => {
    expect(mobileIncidentHref("abc")).toBe("/mobile/incident/?id=abc");
    expect(mobileIncidentHref("abc", true)).toBe("/mobile/incident/?id=abc&silence=1");
  });
});

describe("silenceFor", () => {
  const now = new Date("2026-11-24T02:00:00Z");
  it("silences the check when known, else the server", () => {
    const incident = { server_id: "s1", relay_id: null, metric: "monitorflow_disk_used_percent" };
    expect(silenceFor(incident, { name: "disk", instance: "/" }, 60, now, "phone")).toEqual({
      scope: "server",
      target_id: "s1",
      ends_at: "2026-11-24T03:00:00.000Z",
      reason: "phone",
      checks: [{ name: "disk", instance: "/" }],
    });
    expect(silenceFor(incident, null, 30, now, "phone")).toEqual({
      scope: "server",
      target_id: "s1",
      ends_at: "2026-11-24T02:30:00.000Z",
      reason: "phone",
    });
  });

  it("silences a whole server or relay that went offline", () => {
    expect(silenceFor({ server_id: "s1", metric: "offline" }, { name: "x", instance: "" }, 60, now, "r").checks).toBeUndefined();
    expect(silenceFor({ server_id: null, relay_id: "r1", metric: "offline" }, null, 60, now, "r")).toMatchObject({
      scope: "relay",
      target_id: "r1",
    });
  });
});

describe("onCallNow", () => {
  const turn = (user: string | null, ends = "2026-11-25T08:00:00Z", problem: OnCallTurn["problem"] = null): OnCallTurn => ({
    seat: 0,
    starts_at: "2026-11-24T08:00:00Z",
    ends_at: ends,
    user_id: user,
    username: user,
    override_id: null,
    problem,
  });
  const schedule = (name: string, now: OnCallTurn[], next: OnCallTurn[]): OnCallSchedule =>
    ({ id: name, name, now: now[0] ?? null, next: next[0] ?? null, now_all: now, next_all: next }) as OnCallSchedule;

  it("puts the person's own turns first and names gaps", () => {
    const rows = onCallNow(
      [schedule("Network", [turn("bob")], [turn("carol")]), schedule("Linux", [turn("alice"), turn("dan")], []), schedule("Empty", [], [])],
      "alice",
    );
    expect(rows.map((r) => [r.name, r.me, r.people, r.gap])).toEqual([
      ["Linux", true, ["alice", "dan"], false],
      ["Empty", false, [], true],
      ["Network", false, ["bob"], false],
    ]);
    expect(rows[2]!.next).toEqual(["carol"]);
    expect(onCallNow([schedule("A", [turn("bob", undefined, "no_contact")], [])], "alice")[0]!.gap).toBe(true);
  });
});
