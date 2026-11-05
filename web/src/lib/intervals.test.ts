import { describe, expect, it } from "vitest";

import { alertDelayS, customInterval, defaultTimeoutMs, every, slowAlertNote, splitInterval } from "./intervals";

describe("intervals", () => {
  it("says how often in plain words", () => {
    expect(every(30_000)).toBe("every 30 s");
    expect(every(3_600_000)).toBe("every 1 h");
  });

  it("accepts typed values from 10 seconds to 24 hours", () => {
    expect(customInterval("2", 60_000)).toEqual({ ms: 120_000 });
    expect(customInterval("1,5", 3_600_000)).toEqual({ ms: 5_400_000 });
    expect(customInterval("24", 3_600_000)).toEqual({ ms: 86_400_000 });
    for (const [amount, unit] of [["9", 1_000], ["25", 3_600_000], ["", 1_000], ["x", 60_000], ["-1", 60_000]] as const) {
      expect(customInterval(amount, unit)).toEqual({ error: "Enter a value from 10 seconds to 24 hours." });
    }
  });

  it("prefills Other in the largest unit that fits", () => {
    expect(splitInterval(7_200_000)).toEqual({ amount: "2", unitMs: 3_600_000 });
    expect(splitInterval(90_000)).toEqual({ amount: "90", unitMs: 1_000 });
    expect(splitInterval(300_000)).toEqual({ amount: "5", unitMs: 60_000 });
  });

  it("scales the timeout like the server", () => {
    expect([10_000, 60_000, 300_000, 3_600_000].map(defaultTimeoutMs)).toEqual([10_000, 10_000, 50_000, 60_000]);
  });

  it("says when slow checks delay an alert", () => {
    expect(alertDelayS(300, 3_600)).toBe(7_200);
    expect(alertDelayS(0, 3_600)).toBe(3_600);
    expect(alertDelayS(600, 300)).toBe(900);
    expect(slowAlertNote(300, 60)).toBeNull();
    expect(slowAlertNote(300, undefined)).toBeNull();
    expect(slowAlertNote(0, 3_600)).toBe("Checked every 1 h, so this alert opens up to 1 h after the problem starts.");
    expect(slowAlertNote(600, 300)).toBe("Checked every 5 min, so this alert opens up to 15 min after the problem starts.");
  });
});
