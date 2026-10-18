import { describe, expect, it } from "vitest";

import { formatOs, formatRelative } from "./format";

describe("format", () => {
  it("names operating systems", () => {
    expect(formatOs("debian", "12")).toBe("Debian 12");
    expect(formatOs("rhel", null)).toBe("RHEL");
    expect(formatOs("arch", "rolling")).toBe("arch rolling");
  });

  it("formats times relative to now", () => {
    const now = new Date("2026-10-18T12:00:00Z");
    expect(formatRelative("2026-10-18T11:59:30Z", now)).toBe("just now");
    expect(formatRelative("2026-10-18T11:55:00Z", now)).toBe("5 minutes ago");
    expect(formatRelative("2026-10-18T09:00:00Z", now)).toBe("3 hours ago");
    expect(formatRelative("2026-10-16T12:00:00Z", now)).toBe("2 days ago");
  });
});
