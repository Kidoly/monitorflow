import { describe, expect, it } from "vitest";

import type { EffectiveSensor } from "./api";
import { type SensorValues, formatSensorValue, primaryMetric, sensorFacts, sensorOf, sensorRows, seriesTitle, valuesBySensor } from "./sensors";
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
        : {
            reason: Object.keys(v).length === 0 ? "No data yet" : null,
            series: Object.entries(v).map(([instance, value]) => ({
              instance,
              points: [[at, value]] as [number, number | null][],
              last_value: value,
              last_at: null,
              reason: value === null ? "No data for 15 minutes" : null,
            })),
          },
    ]),
  );

describe("sensor metrics", () => {
  it("stands a custom SNMP check for its first channel, as the server does", () => {
    const custom = (params: Record<string, string>, instance = "central") => ({ ...sensor("snmp_custom", "snmp_fw", instance), params });
    expect(primaryMetric(custom({ oid1: "1.3.6.1.4.1.1.0", name1: "Sessions" }))).toEqual({ metric: "monitorflow_snmp_fw_sessions", instance: "central" });
    expect(primaryMetric(custom({ oid1: "1.3.6.1.4.1.1.0", name1: "RAID state", lookup1: "builtin:if_status", table: "1.3.6.1.4.1.1" }))).toEqual({
      metric: "monitorflow_snmp_fw_raid_state",
    });
    expect(primaryMetric(custom({ oid1: "1.3.6.1.4.1.1.0", name1: "Firmware", type1: "text" })).metric).toBe("monitorflow_snmp_fw_firmware_state");
    expect(primaryMetric(custom({ oid1: "1.3.6.1.4.1.1.0", key1: "in_bits" })).metric).toBe("monitorflow_snmp_fw_in_bits");
    expect(primaryMetric(custom({ oid1: "1.3.6.1.4.1.1.0", name1: "5 min load" })).metric).toBe("monitorflow_snmp_fw_v5_min_load");
  });

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
    expect(primaryMetric(sensor("diskio", "diskio"))).toEqual({ metric: "monitorflow_diskio_busy_ratio" });
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
    const diskio = sensor("diskio", "diskio");
    expect(seriesTitle(diskio, "monitorflow_diskio_busy_ratio", "nvme0n1")).toBe("Disk busy nvme0n1");
    expect(seriesTitle(diskio, "monitorflow_diskio_wait_seconds", "sda")).toBe("Disk wait sda");
    expect(formatSensorValue("monitorflow_diskio_read_bytes_per_second", 3 * 1024 ** 2)).toBe("3 MiB/s");
    expect(formatSensorValue("monitorflow_diskio_reads_per_second", 2.25)).toBe("2.3/s");
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
        incident({ id: "b", metric: "monitorflow_disk_used_ratio", instance: "/var", severity: "critical", last_value: 0.95, reason: "95% used is above the critical limit of 95%" }),
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
    expect(rows[0]).toMatchObject({ value: 0.95, error: null, metric: "monitorflow_disk_used_ratio", reason: "95% used is above the critical limit of 95%" });
    expect(rows[3]).toMatchObject({ reason: null, points: [[at, 0.61]] });
    expect(rows[1]).toMatchObject({ value: 6.8, metric: "monitorflow_cpu_load5" });
    expect(rows[2]).toMatchObject({ value: null, error: "The metrics backend is unavailable.", waiting: false });
  });

  it("marks a sensor without samples unknown", () => {
    const [row] = sensorRows([disk], values([[disk, {}]]), []);
    expect(row).toMatchObject({ title: "Disk used", status: "unknown", waiting: true, reason: "No data yet", error: null });
  });

  it("marks a late series unknown with the server's reason", () => {
    const [row] = sensorRows([disk], values([[disk, { "/": null }]]), []);
    expect(row).toMatchObject({ title: "Disk used /", status: "unknown", reason: "No data for 15 minutes", waiting: false });
  });

  it("matches the batched values to sensors by name and instance", () => {
    const list = { start: 0, end: 3600, step: 120, sensors: [{ name: "disk_data", instance: "data", metric: "m", series: [], reason: "No data yet" }] };
    const map = valuesBySensor([disk, diskData], list);
    expect(map.get(diskData)).toEqual({ series: [], reason: "No data yet" });
    expect(map.get(disk)).toEqual({ series: [], reason: null });
  });
});
