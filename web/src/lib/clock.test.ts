import { describe, expect, it } from "vitest";

import { daysUntil, formatDay, formatMinute, formatWhen } from "./clock";

const now = new Date(2026, 9, 24, 14, 32, 5);

describe("clock", () => {
  it("formats days and minutes", () => {
    expect(formatDay(new Date(2026, 9, 7, 3, 4))).toBe("2026-10-07");
    expect(formatMinute(new Date(2026, 9, 15, 7, 40, 59))).toBe("2026-10-15 07:40");
  });

  it("names today and yesterday", () => {
    expect(formatWhen(new Date(2026, 9, 24, 9, 5), now)).toBe("09:05 today");
    expect(formatWhen(new Date(2026, 9, 23, 22, 5), now)).toBe("yesterday 22:05");
    expect(formatWhen(new Date(2026, 9, 20, 16, 40), now)).toBe("2026-10-20 16:40");
    expect(formatWhen(new Date(2026, 9, 31, 9, 5), now)).toBe("2026-10-31 09:05");
  });

  it("counts days left, rounded up", () => {
    expect(daysUntil(new Date(now.getTime() + 4.2 * 86_400_000), now)).toBe(5);
    expect(daysUntil(new Date(now.getTime() + 1000), now)).toBe(1);
  });
});
