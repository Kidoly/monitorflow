import { describe, expect, it } from "vitest";

import type { EffectiveThreshold } from "./api";
import { thresholdFor } from "./thresholds";

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

  it("reads only plain thresholds, whose values are in the metric's unit", () => {
    const forecast = threshold({ kind: "forecast", instance: "/", warning: 259_200, critical: 86_400 });
    const every = threshold({});
    expect(thresholdFor([forecast, every], DISK, "/")).toBe(every);
  });

  it("does not let an exact instance match another one", () => {
    expect(thresholdFor([threshold({ instance: "/var" })], DISK, "/")).toBeUndefined();
  });
});
