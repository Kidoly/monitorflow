import { describe, expect, it } from "vitest";

import {
  formatBytes,
  formatOs,
  formatPercent,
  formatRelative,
  formatSeconds,
  formatValue,
  unitOf,
} from "./format";

describe("format", () => {
  it("names operating systems", () => {
    expect(formatOs("debian", "12")).toBe("Debian 12");
    expect(formatOs("rhel", null)).toBe("RHEL");
    expect(formatOs("arch", "rolling")).toBe("arch rolling");
    expect(formatOs("windows", "Windows Server 2022 Datacenter 21H2 build 20348.2849")).toBe(
      "Windows Server 2022 Datacenter 21H2 build 20348.2849",
    );
    expect(formatOs("windows", null)).toBe("Windows");
  });

  it("formats times relative to now", () => {
    const now = new Date("2026-10-18T12:00:00Z");
    expect(formatRelative("2026-10-18T11:59:30Z", now)).toBe("just now");
    expect(formatRelative("2026-10-18T11:55:00Z", now)).toBe("5 minutes ago");
    expect(formatRelative("2026-10-18T09:00:00Z", now)).toBe("3 hours ago");
    expect(formatRelative("2026-10-16T12:00:00Z", now)).toBe("2 days ago");
  });

  it("reads the unit from the metric name", () => {
    expect(unitOf("monitorflow_cpu_usage_ratio")).toBe("ratio");
    expect(unitOf("monitorflow_memory_used_bytes")).toBe("bytes");
    expect(unitOf("monitorflow_http_duration_seconds")).toBe("seconds");
    expect(unitOf("monitorflow_cpu_load1")).toBe("number");
  });

  it("formats ratios as percentages and bytes in binary units", () => {
    expect(formatPercent(0.853)).toBe("85.3%");
    expect(formatPercent(1)).toBe("100%");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1536)).toBe("1.5 KiB");
    expect(formatBytes(8 * 1024 ** 3)).toBe("8 GiB");
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(3 * 1024 ** 2, 3)).toBe("0 GiB");
  });

  it("formats values by unit", () => {
    expect(formatValue(0.5, "ratio")).toBe("50%");
    expect(formatValue(2.5 * 1024 ** 4, "bytes")).toBe("2.5 TiB");
    expect(formatValue(90, "seconds")).toBe("1.5 min");
    expect(formatSeconds(0.25)).toBe("250 ms");
    expect(formatValue(1234.567, "number")).toBe("1,234.57");
    expect(formatValue(null, "ratio")).toBe("–");
  });
});
