import { describe, expect, it } from "vitest";

import type { EffectiveSensor, EffectiveThreshold } from "./api";
import {
  CHECK_TYPES,
  checkErrors,
  checkName,
  checkRequests,
  checkTitle,
  checkTypesFor,
  describeThreshold,
  formOf,
  shapeFor,
  thresholdRequest,
} from "./alerts";

const DISK = "monitorflow_disk_used_ratio";

const rule = (overrides: Partial<EffectiveThreshold> = {}): EffectiveThreshold => ({
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
});

const sensor = (kind: string, name: string, params: Record<string, string>): EffectiveSensor => ({
  origin: "server",
  entry_id: "e1",
  kind,
  name,
  instance: "",
  interval_ms: 60_000,
  timeout_ms: null,
  params,
});

const type = (kind: string) => CHECK_TYPES.find((t) => t.kind === kind)!;

describe("thresholds in plain words", () => {
  it("reads the built-in defaults as sentences", () => {
    const say = (r: EffectiveThreshold) => describeThreshold(shapeFor([], r), r);
    expect(say(rule())).toBe("Warn when a disk is over 85% full, critical over 95%, for 5 min.");
    expect(say(rule({ instance: "/data", warning: null, duration_s: 0 }))).toBe("Critical when disk /data is over 95% full.");
    expect(
      say(rule({ metric: "monitorflow_memory_available_ratio", comparison: "<", warning: 0.1, critical: 0.05 })),
    ).toBe("Warn when available memory is under 10%, critical under 5%, for 5 min.");
    expect(say(rule({ metric: "monitorflow_cpu_usage_ratio", instance: "total", warning: 0.9, critical: null, duration_s: 600 }))).toBe(
      "Warn when CPU usage is over 90% for 10 min.",
    );
    expect(say(rule({ enabled: false }))).toBe("No alert on a disk.");
    expect(say(rule({ metric: "monitorflow_cpu_load5", warning: 4, critical: null, duration_s: 0 }))).toBe(
      "Warn when Load average, 5 min is over 4.",
    );
  });

  it("reads a check's alert as an event, unless its values were changed by hand", () => {
    const port = sensor("port", "port_443", { port: "443", host: "127.0.0.1" });
    const closed = rule({ metric: "monitorflow_port_443_up", comparison: "<", warning: null, critical: 1, duration_s: 60 });
    expect(describeThreshold(shapeFor([port], closed), closed)).toBe("Critical when port 443 is closed for 1 min.");
    const log = sensor("log", "log_syslog", { path: "/var/log/syslog", pattern: "error" });
    const lines = rule({ metric: "monitorflow_log_syslog_matches", warning: 0, critical: null, duration_s: 0 });
    expect(describeThreshold(shapeFor([log], lines), lines)).toBe("Warning when /var/log/syslog gets a matching line.");
    const many = { ...lines, warning: 5 };
    expect(shapeFor([log], many).type).toBe("number");
  });
});

describe("the threshold form", () => {
  const shape = shapeFor([], rule());

  it("prefills percentages and minutes, and builds the request", () => {
    const form = formOf(shape, rule());
    expect(form).toEqual({ warning: "85", critical: "95", severity: "critical", minutes: "5" });
    const result = thresholdRequest(shape, rule(), { ...form, warning: "70 %", critical: "", minutes: "2.5" });
    expect(result).toEqual({
      request: {
        metric: DISK,
        instance: null,
        comparison: ">",
        warning: 0.7,
        critical: null,
        duration_s: 150,
        hysteresis: 0.02,
        enabled: true,
      },
    });
  });

  it("says what is wrong, field by field", () => {
    const form = formOf(shape, rule());
    const errors = (changes: Partial<typeof form>) => {
      const result = thresholdRequest(shape, rule(), { ...form, ...changes });
      return "errors" in result ? result.errors : {};
    };
    expect(errors({ warning: "120" }).warning).toMatch(/0 to 100/);
    expect(errors({ warning: "", critical: "" }).warning).toMatch(/warning level, a critical level, or both/);
    expect(errors({ warning: "90", critical: "80" }).critical).toBe("The critical level must be at or above the warning level.");
    expect(errors({ minutes: "-1" }).minutes).toMatch(/0 to 1440/);
    const memory = rule({ metric: "monitorflow_memory_available_ratio", comparison: "<", warning: 0.1, critical: 0.05 });
    const below = thresholdRequest(shapeFor([], memory), memory, { ...formOf(shape, memory), warning: "5", critical: "10" });
    expect(below).toEqual({ errors: { critical: "The critical level must be at or below the warning level." } });
  });

  it("sets an event's fixed value on the chosen severity", () => {
    const port = sensor("port", "port_22", { port: "22" });
    const closed = rule({ metric: "monitorflow_port_22_up", comparison: "<", warning: null, critical: 1, hysteresis: 0 });
    const event = shapeFor([port], closed);
    const result = thresholdRequest(event, closed, { ...formOf(event, closed), severity: "warning", minutes: "0" });
    expect(result).toMatchObject({ request: { comparison: "<", warning: 1, critical: null, duration_s: 0 } });
  });
});

describe("adding a check", () => {
  it("offers what the server's system runs, everything for a site", () => {
    expect(checkTypesFor("debian").map((t) => t.kind)).toEqual(["port", "systemd", "log", "script"]);
    expect(checkTypesFor("windows").map((t) => t.kind)).toEqual(["port", "windows_service", "eventlog", "script"]);
    expect(checkTypesFor(null)).toHaveLength(CHECK_TYPES.length);
  });

  it("names the check after what it watches", () => {
    expect(checkName(type("port"), { port: "443" })).toBe("port_443");
    expect(checkName(type("systemd"), { units: "nginx.service" })).toBe("service_nginx");
    expect(checkName(type("log"), { path: "/var/log/nginx/error.log" })).toBe("log_error");
    expect(checkName(type("script"), { script: "check-backup.sh" })).toBe("script_check_backup");
    expect(checkName(type("eventlog"), { channel: "System" })).toBe("eventlog_system");
    expect(checkName(type("script"), { script: "x".repeat(80) })).toHaveLength(64);
  });

  it("checks the fields before sending", () => {
    expect(checkErrors(type("port"), { port: "", host: "127.0.0.1" })).toEqual({ port: "Enter the port." });
    expect(checkErrors(type("port"), { port: "70000", host: "db" })).toEqual({ port: "Enter a port from 1 to 65535." });
    expect(checkErrors(type("systemd"), { units: "a b" }).units).toMatch(/one name/);
    expect(checkErrors(type("log"), { path: "syslog", pattern: "x" }).path).toMatch(/full path/);
    expect(checkErrors(type("script"), { script: "../x" }).script).toMatch(/file name only/);
    expect(checkErrors(type("port"), { port: "443", host: "127.0.0.1" })).toEqual({});
  });

  it("builds the sensor row and the alert that comes with it", () => {
    expect(checkRequests(type("port"), { port: " 443 ", host: "127.0.0.1" })).toEqual({
      sensor: { kind: "port", name: "port_443", interval_ms: 60_000, params: { port: "443", host: "127.0.0.1" } },
      threshold: {
        metric: "monitorflow_port_443_up",
        instance: null,
        comparison: "<",
        warning: null,
        critical: 1,
        duration_s: 60,
        hysteresis: 0,
        enabled: true,
      },
    });
    const { sensor: row, threshold } = checkRequests(type("eventlog"), { channel: "System" });
    expect(row.params).toEqual({ channel: "System", levels: "critical,error" });
    expect(threshold).toMatchObject({ metric: "monitorflow_eventlog_system_matches", comparison: ">", warning: 0, critical: null });
  });

  it("titles checks in the list", () => {
    expect(checkTitle(sensor("disk", "disk", {}))).toBe("Disk space");
    expect(checkTitle(sensor("port", "port_5432", { port: "5432", host: "db.lan" }))).toBe("Port 5432 on db.lan");
    expect(checkTitle(sensor("systemd", "service_nginx", { units: "nginx" }))).toBe("Service nginx");
    expect(checkTitle(sensor("log", "log_syslog", { path: "/var/log/syslog", pattern: "error" }))).toBe("“error” in /var/log/syslog");
  });
});
