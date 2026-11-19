import { describe, expect, it, vi } from "vitest";

import { browserTimeZone, formatDay, formatMinute, formatWhen, localToIso, toLocalInput } from "./clock";

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

  it("reads and writes datetime-local values in local time", () => {
    const date = new Date(2026, 9, 7, 3, 4, 59);
    expect(toLocalInput(date)).toBe("2026-10-07T03:04");
    expect(localToIso(toLocalInput(date))).toBe(new Date(2026, 9, 7, 3, 4).toISOString());
    expect(localToIso("")).toBeNull();
    expect(localToIso("nonsense")).toBeNull();
  });

  it("names the browser's time zone, UTC when it names none", () => {
    expect(browserTimeZone()).toBe(Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC");
    const spy = vi.spyOn(Intl.DateTimeFormat.prototype, "resolvedOptions").mockReturnValue({ timeZone: "" } as Intl.ResolvedDateTimeFormatOptions);
    try {
      expect(browserTimeZone()).toBe("UTC");
    } finally {
      spy.mockRestore();
    }
  });
});
