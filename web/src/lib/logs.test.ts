import { describe, expect, it } from "vitest";

import { lineSource, logWindow, MAX_WINDOW_MS, priorityTone, priorityWord } from "./logs";
import { incidentLogsHref } from "./routes";

const NOW = new Date("2026-10-22T14:32:05Z");

describe("logs", () => {
  it("names priorities in words", () => {
    expect([0, 3, 4, 6, 9, null].map((p) => priorityWord(p))).toEqual([
      "Emergency",
      "Error",
      "Warning",
      "Info",
      "Unknown",
      "Unknown",
    ]);
    expect([2, 3, 4, 5, null].map((p) => priorityTone(p))).toEqual(["bad", "bad", "warn", "neutral", "neutral"]);
  });

  it("shows the unit, else the identifier", () => {
    const line = { time: "", message: "", priority: 4, pid: null };
    expect(lineSource({ ...line, unit: "nginx.service", identifier: "nginx" })).toBe("nginx.service");
    expect(lineSource({ ...line, unit: null, identifier: "kernel" })).toBe("kernel");
    expect(lineSource({ ...line, unit: null, identifier: null })).toBe("");
  });

  it("picks the window from the URL, the last hour by default", () => {
    expect(logWindow(null, null, NOW)).toEqual({ start: new Date("2026-10-22T13:32:05Z"), end: NOW, live: true });
    const from = "2026-10-22T09:45:00.000Z";
    expect(logWindow(from, null, NOW)).toEqual({ start: new Date(from), end: NOW, live: true });
    const to = "2026-10-22T10:30:00.000Z";
    expect(logWindow(from, to, NOW)).toEqual({ start: new Date(from), end: new Date(to), live: false });
    // Nonsense falls back to the last hour; a long window is cut.
    expect(logWindow("soon", "later", NOW).start).toEqual(new Date("2026-10-22T13:32:05Z"));
    expect(logWindow(to, from, NOW).start).toEqual(new Date("2026-10-22T08:45:00Z"));
    const year = logWindow("2025-01-01T00:00:00Z", null, NOW);
    expect(NOW.getTime() - year.start.getTime()).toBe(MAX_WINDOW_MS);
  });

  it("links an incident to the logs around it", () => {
    const open = new URL(incidentLogsHref("s1", "2026-10-22T10:00:00Z", null), "http://x");
    expect(open.pathname).toBe("/servers/view/");
    expect(Object.fromEntries(open.searchParams)).toEqual({ id: "s1", tab: "logs", from: "2026-10-22T09:45:00.000Z" });
    const resolved = new URL(incidentLogsHref("s1", "2026-10-22T10:00:00Z", "2026-10-22T10:20:00Z"), "http://x");
    expect(resolved.searchParams.get("to")).toBe("2026-10-22T10:35:00.000Z");
  });
});
