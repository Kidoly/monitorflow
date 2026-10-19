import { describe, expect, it } from "vitest";

import type { EffectiveSensor } from "./api";
import { rollupFor, sensorInterval } from "./metrics";

function sensor(name: string, interval_ms: number, instance = ""): EffectiveSensor {
  return { origin: "default", entry_id: null, kind: name, name, instance, interval_ms, timeout_ms: null, params: {} };
}

describe("rollupFor", () => {
  it("keeps the worst value of each step", () => {
    const cases: Record<string, string> = {
      cpu_usage_ratio: "max",
      cpu_load5: "max",
      memory_used_bytes: "max",
      disk_used_ratio: "max",
      port_connect_seconds: "max",
      backup_status: "max",
      systemd_failed: "max",
      systemd_restarts: "max",
      nginx_errors_matches: "max",
      nginx_errors_lag_bytes: "max",
      memory_available_ratio: "min",
      memory_available_bytes: "min",
      disk_free_bytes: "min",
      disk_inodes_free: "min",
      memory_swap_free_bytes: "min",
      port_up: "min",
      systemd_active: "min",
      systemd_loaded: "min",
      disk_total_bytes: "avg",
      disk_inodes: "avg",
      backup_files: "avg",
    };
    for (const [metric, rollup] of Object.entries(cases)) {
      expect([metric, rollupFor(`monitorflow_${metric}`)]).toEqual([metric, rollup]);
    }
  });
});

describe("sensorInterval", () => {
  it("uses the most specific sensor's longest interval", () => {
    const sensors = [sensor("disk", 60_000), sensor("disk_data", 600_000), sensor("port", 30_000, "22"), sensor("port", 120_000, "443")];
    expect(sensorInterval(sensors, "monitorflow_disk_used_ratio")).toBe(60);
    expect(sensorInterval(sensors, "monitorflow_disk_data_used_ratio")).toBe(600);
    expect(sensorInterval(sensors, "monitorflow_port_up")).toBe(120);
    expect(sensorInterval(sensors, "monitorflow_cpu_usage_ratio")).toBeUndefined();
    expect(sensorInterval([], "monitorflow_cpu_usage_ratio")).toBeUndefined();
  });
});
