import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import { formatAgo, formatDay, formatDuration, formatSince } from "./time";

describe("durations and days", () => {
  it("writes durations in the two largest units", () => {
    expect(formatDuration(42.7)).toBe("42 s");
    expect(formatDuration(34 * 60 + 5)).toBe("34 min");
    expect(formatDuration(2 * 3600 + 52 * 60)).toBe("2 h 52 min");
    expect(formatDuration(3 * 3600)).toBe("3 h");
    expect(formatDuration(3 * 86_400 + 4 * 3600 + 59)).toBe("3 d 4 h");
    expect(formatDuration(-5)).toBe("0 s");
  });

  it("writes moments relative to today's date", () => {
    const now = new Date(2026, 9, 22, 14, 32);
    expect(formatDay(new Date(2026, 9, 22, 10, 12), now)).toBe("10:12 today");
    expect(formatDay(new Date(2026, 9, 21, 23, 10), now)).toBe("Yesterday 23:10");
    expect(formatDay(new Date(2026, 9, 20, 8, 0), now)).toBe("20 Oct 08:00");
    expect(formatDay(new Date(2025, 11, 30, 8, 0), now)).toBe("2025-12-30 08:00");
    expect(formatSince(new Date(2026, 9, 22, 11, 40), now)).toBe("11:40");
    expect(formatSince(new Date(2026, 9, 21, 22, 5), now)).toBe("Yesterday 22:05");
  });

  it("speaks the active language", () => {
    setActiveLocale("fr");
    const now = new Date(2026, 8, 22, 14, 32);
    expect(formatDuration(3 * 86_400 + 4 * 3600)).toBe("3 j 4 h");
    expect(formatAgo(new Date(2026, 8, 22, 14, 31, 48), now)).toBe("il y a 12 s");
    expect(formatAgo(new Date(2026, 8, 22, 14, 42), now)).toBe("dans 10 min");
    expect(formatDay(new Date(2026, 8, 22, 10, 12), now)).toBe("aujourd’hui 10:12");
    expect(formatDay(new Date(2026, 8, 21, 23, 10), now)).toBe("hier 23:10");
    expect(formatDay(new Date(2026, 8, 4, 8, 0), now)).toBe("04 sept. 08:00");
  });
});
