import { describe, expect, it } from "vitest";

import { handoffRule, move, placeName, problemText, timeZones, who } from "./onCall";

describe("onCall", () => {
  it("says when hand-offs happen", () => {
    expect(handoffRule({ rotation: "weekly", handoff_day: 5, handoff_time: "17:30", time_zone: "America/New_York" })).toBe(
      "Hands off every Friday at 17:30, America/New York time",
    );
    expect(handoffRule({ rotation: "daily", handoff_day: null, handoff_time: "09:00", time_zone: "UTC" })).toBe(
      "Hands off every day at 09:00, UTC time",
    );
  });

  it("names the place like the channels do", () => {
    expect(placeName({ customer_name: "acme", site_name: "paris" }, true)).toBe("acme / paris");
    expect(placeName({ customer_name: "acme", site_name: "paris" }, false)).toBe("paris");
    expect(placeName({ customer_name: "acme", site_name: null }, false)).toBe("All of acme");
    expect(placeName({ customer_name: null, site_name: null }, false)).toBe("Everything");
    // Without customers (ADR-0104).
    expect(placeName({ customer_name: "acme", site_name: null }, false, false)).toBe("All sites");
    expect(problemText("no_access", "bob", "All sites")).toBe("bob can no longer see all sites");
  });

  it("explains every gap in plain words", () => {
    expect(problemText("no_access", "bob", "All of acme")).toBe("bob can no longer see All of acme");
    expect(problemText("no_access", "bob", "Everything")).toBe("bob can no longer see everything");
    expect(problemText("disabled", "bob", "x")).toBe("bob's account is disabled");
    expect(problemText("removed", null, "x")).toBe("This turn is a removed user's");
    expect(problemText("no_mail_server", "bob", "x")).toContain("no email channel");
    expect(who({ username: null, user_id: null })).toBe("nobody");
    expect(who({ username: null, user_id: "x" })).toBe("a removed user");
  });

  it("moves people within the list", () => {
    expect(move(["a", "b", "c"], 2, -1)).toEqual(["a", "c", "b"]);
    expect(move(["a", "b", "c"], 0, -1)).toEqual(["a", "b", "c"]);
    expect(move(["a", "b", "c"], 0, 5)).toEqual(["b", "c", "a"]);
  });

  it("always offers the current zone and UTC", () => {
    const zones = timeZones("Mars/Olympus");
    expect(zones).toContain("Mars/Olympus");
    expect(zones).toContain("UTC");
    expect(new Set(zones).size).toBe(zones.length);
  });
});
