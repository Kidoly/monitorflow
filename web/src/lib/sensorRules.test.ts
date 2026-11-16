import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { EffectiveSensor, EffectiveThreshold, SensorConfigEntry } from "./api";
import { shapeOf } from "./alerts";
import {
  LEVEL_LABELS,
  applyToChoices,
  applyToFor,
  ownOverrides,
  perInstance,
  ruleLevel,
  ruleOf,
  ruleCompact,
  ruleSummary,
  rowRule,
  thresholdInstance,
} from "./sensorRules";

const sensor = (s: Partial<EffectiveSensor>): EffectiveSensor => ({
  origin: "default",
  entry_id: null,
  kind: "disk",
  name: "disk",
  instance: "",
  interval_ms: 60_000,
  timeout_ms: null,
  params: {},
  ...s,
});
const disk = sensor({});
const cpu = sensor({ kind: "cpu", name: "cpu" });
const diskio = sensor({ kind: "diskio", name: "diskio" });
const port = sensor({ kind: "port", name: "port_443", origin: "server", entry_id: "p1", params: { port: "443" } });

const rule = (t: Partial<EffectiveThreshold>): EffectiveThreshold => ({
  origin: "default",
  threshold_id: null,
  metric: "monitorflow_disk_used_ratio",
  instance: null,
  enabled: true,
  comparison: ">",
  warning: 0.85,
  critical: 0.95,
  duration_s: 300,
  hysteresis: 0.02,
  ...t,
});

describe("sensor rules", () => {
  it("tells one disk of several from the one instance a sensor shows", () => {
    expect(perInstance(disk, "/var")).toBe(true);
    expect(perInstance(disk, "")).toBe(false);
    expect(perInstance(cpu, "total")).toBe(false);
    expect(perInstance(sensor({ instance: "/data", name: "disk_data" }), "/data")).toBe(false);
  });

  it("finds the exact instance's threshold first, muted or not", () => {
    const all = rule({ threshold_id: "a" });
    const muted = rule({ threshold_id: "b", instance: "/boot", enabled: false });
    expect(ruleOf([all, muted], "monitorflow_disk_used_ratio", "/boot")).toBe(muted);
    expect(ruleOf([all, muted], "monitorflow_disk_used_ratio", "/")).toBe(all);
  });

  it("shows disk activity's wait alert on its busy row", () => {
    const wait = rule({ metric: "monitorflow_diskio_wait_seconds", warning: 0.2, critical: null });
    expect(rowRule([diskio], [wait], diskio, "monitorflow_diskio_busy_ratio", "sda")).toBe(wait);
  });

  it("badges the most specific level of the timing and the alert", () => {
    expect(ruleLevel(disk, rule({}), true)).toBe("builtin");
    expect(ruleLevel(disk, rule({ origin: "site" }), true)).toBe("site");
    expect(ruleLevel(sensor({ origin: "server" }), rule({ origin: "site" }), true)).toBe("server");
    expect(ruleLevel(disk, rule({ origin: "server", instance: "/var" }), true)).toBe("sensor");
    expect(ruleLevel(cpu, rule({ origin: "server", instance: "total" }), false)).toBe("server");
    expect(ruleLevel(port, undefined, false)).toBe("server");
  });

  it("sums a rule up in plain words", () => {
    const percent = shapeOf([disk], "monitorflow_disk_used_ratio", null);
    expect(ruleSummary(percent, rule({ warning: 0.7, critical: 0.8 }), 60_000)).toBe(
      "every 1 min · warn > 70% · crit > 80% · for 5 min",
    );
    expect(ruleSummary(percent, rule({ enabled: false }), 30_000)).toBe("every 30 s · alert turned off");
    expect(ruleSummary(percent, undefined, 300_000)).toBe("every 5 min · no alert");
    expect(ruleCompact(percent, rule({ warning: 0.7, critical: 0.8 }), 60_000)).toBe("1 min · > 70% / 80% · 5 min");
    expect(ruleCompact(percent, rule({ warning: null, critical: 0.8 }), 30_000)).toBe("30 s · > 80% · 5 min");
    expect(ruleCompact(percent, rule({ enabled: false }), 30_000)).toBe("30 s · alert turned off");
    const event = shapeOf([port], "monitorflow_port_443_up", null);
    const up = rule({ metric: "monitorflow_port_443_up", comparison: "<", warning: null, critical: 1, duration_s: 0 });
    expect(ruleSummary(event, up, 60_000)).toBe("every 1 min · critical when it fails");
  });

  it("speaks French", () => {
    setActiveLocale("fr");
    const percent = shapeOf([disk], "monitorflow_disk_used_ratio", null);
    expect(ruleSummary(percent, rule({ warning: 0.7, critical: 0.8 }), 60_000)).toBe(
      "toutes les 1 min · avert. > 70\u00a0% · crit. > 80\u00a0% · pendant 5 min",
    );
    expect(applyToChoices(disk, "/var", "web-1", "paris").map((c) => c.label)).toEqual([
      "ce capteur uniquement (/var)",
      "chaque disque sur web-1",
      "chaque serveur de paris (défaut du site)",
    ]);
    expect(LEVEL_LABELS.site).toBe("Défaut du site");
  });

  it("offers this sensor only where it has several instances, and preselects the level in effect", () => {
    expect(applyToChoices(disk, "/var", "web-1", "paris").map((c) => c.label)).toEqual([
      "this sensor only (/var)",
      "every disk on web-1",
      "every server of paris (site default)",
    ]);
    expect(applyToChoices(cpu, "total", "web-1", "paris").map((c) => c.value)).toEqual(["server", "site"]);
    expect(applyToFor("builtin", true)).toBe("server");
    expect(applyToFor("site", true)).toBe("site");
    expect(applyToFor("sensor", true)).toBe("sensor");
  });

  it("writes one instance's threshold for this sensor, every instance's otherwise, and keeps CPU's total", () => {
    const place = { serverId: "s", siteId: "site", sensor: disk, instance: "/var" };
    expect(thresholdInstance("sensor", place, null)).toBe("/var");
    expect(thresholdInstance("server", place, "/var")).toBeNull();
    expect(thresholdInstance("site", { ...place, sensor: cpu, instance: "total" }, "total")).toBe("total");
  });

  it("resets the most specific override first, and never a check added on the server", () => {
    const place = { serverId: "s", siteId: "site", sensor: sensor({ origin: "server", entry_id: "r1" }), instance: "/var" };
    const row = { id: "r1", name: "disk", instance: "", enabled: true } as SensorConfigEntry;
    const own = rule({ origin: "server", threshold_id: "t1", instance: "/var" });
    expect(ownOverrides(place, own, [row], [disk])).toEqual({ thresholdId: "t1", sensorRowId: null, level: "sensor" });
    const server = rule({ origin: "server", threshold_id: "t2" });
    expect(ownOverrides(place, server, [row], [disk])).toEqual({ thresholdId: "t2", sensorRowId: "r1", level: "server" });
    expect(ownOverrides(place, rule({ origin: "site", threshold_id: "t3" }), [], [disk]).level).toBeNull();
    const added = { ...place, sensor: port, instance: "" };
    expect(ownOverrides(added, rule({ origin: "server", threshold_id: "t4" }), [], []).level).toBeNull();
  });
});
