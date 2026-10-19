import { describe, expect, it } from "vitest";

import type { EffectiveThreshold } from "./api";
import { thresholdFor, thresholdLines } from "./thresholds";

const DISK = "monitorflow_disk_used_ratio";

function threshold(overrides: Partial<EffectiveThreshold>): EffectiveThreshold {
  return {
    origin: "default",
    threshold_id: null,
    metric: DISK,
    instance: null,
    enabled: true,
    comparison: ">",
    warning: 0.85,
    critical: 0.95,
    duration_s: 300,
    hysteresis: 0.02,
    ...overrides,
  };
}

describe("thresholdFor", () => {
  it("prefers the exact instance, even muted, over every instance", () => {
    const every = threshold({});
    const boot = threshold({ origin: "server", instance: "/boot", enabled: false });
    const data = threshold({ origin: "site", instance: "/data", warning: 0.7 });
    const all = [every, boot, data];
    expect(thresholdFor(all, DISK, "/")).toBe(every);
    expect(thresholdFor(all, DISK, "/data")).toBe(data);
    expect(thresholdFor(all, DISK, "/boot")).toBeUndefined();
    expect(thresholdFor(all, "monitorflow_cpu_usage_ratio", "/")).toBeUndefined();
  });

  it("skips a muted entry for every instance", () => {
    expect(thresholdFor([threshold({ enabled: false })], DISK, "/")).toBeUndefined();
  });

  it("does not let an exact instance match another one", () => {
    expect(thresholdFor([threshold({ instance: "/var" })], DISK, "/")).toBeUndefined();
  });
});

describe("thresholdLines", () => {
  it("draws shared values once with plain labels", () => {
    expect(thresholdLines([threshold({})], DISK, ["/", "/var", "/home"])).toEqual([
      { severity: "warning", value: 0.85, label: "Warning" },
      { severity: "critical", value: 0.95, label: "Critical" },
    ]);
  });

  it("names the instances of values not every line shares", () => {
    const lines = thresholdLines(
      [threshold({ critical: null }), threshold({ instance: "/data", warning: 0.7, critical: 0.9 })],
      DISK,
      ["/", "/var", "/data"],
    );
    expect(lines).toEqual([
      { severity: "warning", value: 0.85, label: "Warning (/, /var)" },
      { severity: "warning", value: 0.7, label: "Warning (/data)" },
      { severity: "critical", value: 0.9, label: "Critical (/data)" },
    ]);
  });

  it("leaves out muted instances and shortens long lists", () => {
    const lines = thresholdLines(
      [threshold({ critical: null }), threshold({ instance: "/boot", enabled: false })],
      DISK,
      ["/", "/a", "/b", "/c", "/boot"],
    );
    expect(lines).toEqual([{ severity: "warning", value: 0.85, label: "Warning (/, /a and 2 more)" }]);
  });

  it("matches the series without an instance", () => {
    const port = "monitorflow_port_open";
    const t = threshold({ metric: port, instance: "", comparison: "<", warning: null, critical: 1 });
    expect(thresholdLines([t], port, [""])).toEqual([{ severity: "critical", value: 1, label: "Critical" }]);
  });

  it("draws nothing for a chart without lines or thresholds", () => {
    expect(thresholdLines([threshold({})], DISK, [])).toEqual([]);
    expect(thresholdLines([], DISK, ["/"])).toEqual([]);
  });
});
