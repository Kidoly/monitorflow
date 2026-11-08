import { describe, expect, it } from "vitest";

import { fromLocalInput, inOneHour, silenceTitle, tomorrowMorning } from "./silences";

describe("silences", () => {
  it("puts one hour after now and tomorrow at 8:00 local", () => {
    const now = new Date(2026, 10, 30, 22, 15);
    expect(inOneHour(now)).toEqual(new Date(2026, 10, 30, 23, 15));
    expect(tomorrowMorning(now)).toEqual(new Date(2026, 11, 1, 8));
    // Past midnight, tomorrow is still the next day.
    expect(tomorrowMorning(new Date(2026, 10, 3, 1))).toEqual(new Date(2026, 10, 4, 8));
  });

  it("reads a form's date", () => {
    expect(fromLocalInput("")).toBeNull();
    expect(fromLocalInput("nonsense")).toBeNull();
    expect(fromLocalInput("2026-11-03T08:00")).toEqual(new Date(2026, 10, 3, 8));
  });

  it("names what is silenced", () => {
    expect(silenceTitle({ scope: "server", target_name: "web-1" })).toBe("web-1");
    expect(silenceTitle({ scope: "relay", target_name: "gw" })).toBe("relay gw");
    expect(silenceTitle({ scope: "customer", target_name: "acme" }, false)).toBe("all sites");
    expect(silenceTitle({ scope: "site", target_name: "paris" })).toBe("site paris");
    expect(silenceTitle({ scope: "customer", target_name: "acme" })).toBe("customer acme");
    expect(silenceTitle({ scope: "all", target_name: null })).toBe("everything");
  });
});
