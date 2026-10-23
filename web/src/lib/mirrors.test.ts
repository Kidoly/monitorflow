import { describe, expect, it } from "vitest";

import { mirror } from "@/test/mirrors";

import { describeMirror, durationText, intervalText, nextSync, shortTime, upstreamText } from "./mirrors";

describe("mirror helpers", () => {
  it("describes a mirror on one line", () => {
    expect(upstreamText("https://deb.debian.org/debian/")).toBe("deb.debian.org/debian");
    expect(describeMirror(mirror())).toBe("apt · deb.debian.org/debian · main contrib · amd64 arm64");
    expect(describeMirror(mirror(), { distribution: true })).toBe(
      "apt · deb.debian.org/debian · bookworm · main contrib · amd64 arm64",
    );
    expect(describeMirror(mirror({ family: "dnf", distribution: "", components: [], architectures: [] }))).toBe(
      "dnf · deb.debian.org/debian · all architectures",
    );
  });

  it("writes intervals in their largest whole unit", () => {
    expect(intervalText(43_200)).toBe("every 12 h");
    expect(intervalText(86_400)).toBe("every 1 d");
    expect(intervalText(900)).toBe("every 15 min");
    expect(intervalText(5_400)).toBe("every 90 min");
    expect(intervalText(null)).toBe("manual syncs only");
  });

  it("puts the next sync one interval after the last one started", () => {
    const m = { sync_interval_s: 3600 };
    expect(nextSync(m, { started_at: "2026-10-22T08:00:00Z" }, false)?.toISOString()).toBe("2026-10-22T09:00:00.000Z");
    expect(nextSync(m, null, false)?.getTime()).toBe(0);
    expect(nextSync(m, undefined, false)).toBeNull();
    expect(nextSync(m, { started_at: "2026-10-22T08:00:00Z" }, true)).toBeNull();
    expect(nextSync({ sync_interval_s: null }, null, false)).toBeNull();
  });

  it("formats times and durations like the handoff", () => {
    const now = new Date(2026, 9, 22, 14, 0);
    expect(shortTime(new Date(2026, 9, 22, 20, 30), now)).toBe("20:30");
    expect(shortTime(new Date(2026, 9, 23, 2, 0), now)).toBe("23 Oct 02:00");
    expect(durationText(42_000)).toBe("42 s");
    expect(durationText(252_000)).toBe("4 min 12 s");
    expect(durationText(542_000)).toBe("9 min 02 s");
    expect(durationText(3_900_000)).toBe("1 h 05 min");
  });
});
