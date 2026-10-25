import { describe, expect, it } from "vitest";

import type { EffectiveSensor } from "./api";
import { type SensorValues, formatSensorValue, primaryMetric, sensorFacts, sensorOf, sensorRows, seriesTitle } from "./sensors";
import { incident } from "@/test/incidents";

const sensor = (kind: string, name = kind, instance = "", interval_ms = 30_000): EffectiveSensor => ({
  origin: "default",
  entry_id: null,
  kind,
  name,
  instance,
  interval_ms,
  timeout_ms: kind === "disk" ? 10_000 : null,
  params: {},
});

const cpu = sensor("cpu", "cpu", "", 15_000);
const disk = sensor("disk");
const diskData = sensor("disk", "disk_data", "data");
const nginx = sensor("systemd", "systemd", "nginx.service");

const at = 1_800_000_000;
const values = (entries: [EffectiveSensor, Record<string, number | null> | string][]) =>
  new Map<EffectiveSensor, SensorValues>(
    entries.map(([s, v]): [EffectiveSensor, SensorValues] => [
      s,
      typeof v === "string"
        ? { error: v }
        : { series: Object.entries(v).map(([instance, value]) => ({ labels: instance ? { instance } : ({} as Record<string, string>), points: [[at, value]] as [number, number | null][] })) },
    ]),
  );

describe("sensor metrics", () => {
  it("picks the metric that stands for each kind", () => {
    expect(primaryMetric(cpu)).toEqual({ metric: "monitorflow_cpu_usage_ratio", instance: "total" });
    expect(primaryMetric(disk)).toEqual({ metric: "monitorflow_disk_used_ratio" });
    expect(primaryMetric(diskData)).toEqual({ metric: "monitorflow_disk_data_used_ratio", instance: "data" });
    expect(primaryMetric(nginx)).toEqual({ metric: "monitorflow_systemd_active", instance: "nginx.service" });
    expect(primaryMetric(sensor("custom", "backup"))).toEqual({ metric: "monitorflow_backup" });
    expect(primaryMetric(sensor("windows_service", "services", "W32Time"))).toEqual({
      metric: "monitorflow_services_active",
      instance: "W32Time",
    });
    expect(primaryMetric(sensor("eventlog", "app_errors"))).toEqual({ metric: "monitorflow_app_errors_matches" });
  });

  it("finds the sensor of a metric by its longest prefix", () => {
    const all = [disk, diskData, cpu];
    expect(sensorOf(all, "monitorflow_disk_data_used_ratio")).toBe(diskData);
    expect(sensorOf(all, "monitorflow_disk_used_ratio")).toBe(disk);
    expect(sensorOf(all, "monitorflow_cpu_load5")).toBe(cpu);
    expect(sensorOf(all, "monitorflow_memory_used_bytes")).toBeUndefined();
  });

  it("titles rows and formats their values", () => {
    expect(seriesTitle(disk, "monitorflow_disk_used_ratio", "/var")).toBe("Disk used /var");
    expect(seriesTitle(cpu, "monitorflow_cpu_usage_ratio", "total")).toBe("CPU usage");
    expect(seriesTitle(nginx, "monitorflow_systemd_active", "nginx.service")).toBe("nginx.service");
    const w32time = sensor("windows_service", "services");
    expect(seriesTitle(w32time, "monitorflow_services_active", "W32Time")).toBe("W32Time");
    expect(seriesTitle(sensor("eventlog", "app_errors"), "monitorflow_app_errors_matches", "")).toBe("app_errors");
    expect(formatSensorValue("monitorflow_services_installed", 0)).toBe("not installed");
    expect(seriesTitle(undefined, "monitorflow_cpu_load5", "")).toBe("Load average, 5 min");
    expect(formatSensorValue("monitorflow_systemd_active", 1)).toBe("active");
    expect(formatSensorValue("monitorflow_port_up", 0)).toBe("closed");
    expect(formatSensorValue("monitorflow_disk_used_ratio", 0.942)).toBe("94.2%");
    expect(formatSensorValue("monitorflow_disk_used_ratio", null)).toBe("–");
    expect(sensorFacts(disk, "/var")).toBe("disk · /var · every 30 s · timeout 10 s");
    expect(sensorFacts(cpu, "total")).toBe("cpu · every 15 s");
  });
});

describe("sensorRows", () => {
  it("makes one row per instance, with the worst open incident, sorted by status then title", () => {
    const rows = sensorRows(
      [cpu, disk, nginx],
      values([
        [cpu, { total: 0.38 }],
        [disk, { "/": 0.61, "/var": 0.942 }],
        [nginx, "The metrics backend is unavailable."],
      ]),
      [
        incident({ metric: "monitorflow_disk_used_ratio", instance: "/var", severity: "warning" }),
        incident({ id: "b", metric: "monitorflow_disk_used_ratio", instance: "/var", severity: "critical", last_value: 0.95 }),
        // Load average belongs to the CPU sensor's single row.
        incident({ id: "c", metric: "monitorflow_cpu_load5", instance: "", severity: "warning", last_value: 6.8 }),
      ],
    );
    expect(rows.map((r) => [r.title, r.status])).toEqual([
      ["Disk used /var", "critical"],
      ["CPU usage", "warning"],
      ["nginx.service", "unknown"],
      ["Disk used /", "ok"],
    ]);
    expect(rows[0]).toMatchObject({ value: 0.95, error: null, metric: "monitorflow_disk_used_ratio" });
    expect(rows[1]).toMatchObject({ value: 6.8, metric: "monitorflow_cpu_load5" });
    expect(rows[2]).toMatchObject({ value: null, error: "The metrics backend is unavailable." });
  });

  it("marks a sensor without samples unknown", () => {
    const [row] = sensorRows([disk], values([[disk, {}]]), []);
    expect(row).toMatchObject({ title: "Disk used", status: "unknown", error: expect.stringMatching(/No data yet/) });
  });
});
