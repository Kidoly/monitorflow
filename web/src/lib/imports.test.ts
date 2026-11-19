import { beforeEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { ImportItem } from "./api";
import { countsText, grouped, itemTitle, leftOut, noteText, originKind, toCreate } from "./imports";

const item = (id: string, needs: string[] = [], extra: Partial<ImportItem> = {}): ImportItem => ({
  id,
  kind: "check",
  list: "exact",
  origin: { kind: "item", name: id },
  hosts: [],
  present: false,
  needs,
  create: { object: "check", server: "a", kind: "port", name: "port_22", interval_ms: 60000, params: { port: "22" } },
  ...extra,
});

describe("imports", () => {
  beforeEach(() => setActiveLocale("en"));

  it("leaves out what needs an item left out, transitively", () => {
    const items = [item("server"), item("check", ["server"]), item("alert", ["check", "server"]), item("other")];
    expect([...leftOut(items, new Set(["server"]))]).toEqual(["server", "check", "alert"]);
    expect(toCreate(items, leftOut(items, new Set(["check"])))).toBe(2);
    const present = [item("p", [], { present: true }), item("n", [], { list: "not_movable", create: undefined })];
    expect(toCreate(present, new Set())).toBe(0);
  });

  it("translates notes and kinds, and falls back to the server's words", () => {
    expect(noteText({ code: "builtin_net", text: "english" })).toBe("Every server measures the traffic of its network interfaces already.");
    expect(noteText({ code: "from_a_newer_server", text: "As the server says." })).toBe("As the server says.");
    expect(originKind("trigger")).toBe("Trigger");
    expect(originKind("sensor_group")).toBe("sensor_group");
    setActiveLocale("fr");
    expect(noteText({ code: "web_password", args: { username: "monitor" }, text: "" })).toContain("le mot de passe de monitor");
  });

  it("titles what items create", () => {
    const threshold: ImportItem = item("t", [], {
      kind: "threshold",
      create: {
        object: "threshold",
        server: "a",
        metric: "monitorflow_cpu_usage_ratio",
        instance: "total",
        comparison: ">",
        warning: 0.8,
        critical: 0.95,
        duration_s: 300,
        hysteresis: 0,
      },
    });
    const checks = [item("c")];
    expect(itemTitle(checks[0]!, checks)).toBe("Port 22");
    expect(itemTitle(threshold, [threshold])).toMatch(/80%.*95%/);
    const channel = item("ch", [], {
      kind: "channel",
      create: { object: "channel", kind: "email", settings: { to: ["ops@example.com"] }, has_secret: true, site_id: null },
    });
    expect(itemTitle(channel, [])).toBe("Email to ops@example.com");
    const service = item("sv", [], {
      kind: "service",
      create: {
        object: "service",
        name: "Shop",
        at_least: 1,
        target: 99.9,
        members: [{ server: "web-01" }, { server: "db-01", check: { name: "port_5432", instance: "" } }],
      },
    });
    expect(itemTitle(service, [])).toBe("Shop: up while at least 1 of web-01, db-01 port_5432 is, target 99.9 %");
    const dependency = item("dependency:app-01/port_443:sw-core", [], {
      kind: "dependency",
      hosts: ["app-01", "sw-core"],
      create: { object: "dependency", server: "app-01", check: { name: "port_443", instance: "" }, parent: "sw-core" },
    });
    expect(itemTitle(dependency, [])).toBe("port_443 on app-01 depends on sw-core");
    const window = item("window:Patch", [], {
      kind: "window",
      create: {
        object: "window", servers: ["web-01"], checks: [{ name: "service_nginx", instance: "" }],
        repeat: "monthly", every: 1, weekdays: ["tuesday"], month_day: null, month_week: 2, months: [],
        start_time: "22:00", duration_minutes: 240, time_zone: "Europe/Paris", starts_on: null, ends_on: null,
        reason: "Patch night",
      },
    });
    expect(itemTitle(window, [])).toBe(
      "On the second Tuesday of every month, 22:00 to 02:00 the next day (Europe/Paris), service_nginx: Patch night",
    );
  });

  it("titles network scans, from the central server or a relay", () => {
    const scan = (every: "day" | null, probe: string) =>
      item("network_scan:Office", [], {
        kind: "network_scan",
        create: {
          object: "network_scan", name: "Office", targets: ["192.0.2.0/24"], addresses: 254,
          ports: [22], every, probe, has_snmp: true,
        },
      });
    expect(itemTitle(scan("day", "central"), [])).toBe(
      "Office: 254 addresses, every day, from the central server",
    );
    expect(itemTitle(scan(null, "relay-paris"), [])).toBe("Office: 254 addresses, when asked, from relay-paris");
    setActiveLocale("fr");
    expect(itemTitle(scan("day", "central"), [])).toBe(
      "Office\u00a0: 254 adresses, tous les jours, depuis le serveur central",
    );
  });

  it("groups by list and kind, in order", () => {
    const items = [item("c"), item("s", [], { kind: "server" }), item("x", [], { list: "approximated" })];
    expect(grouped(items, "exact").map((g) => g.kind)).toEqual(["server", "check"]);
    expect(grouped(items, "not_movable")).toEqual([]);
    expect(countsText({ server: 1, check: 2 })).toBe("1 server, 2 checks");
    expect(countsText({ server: 1, network_scan: 2 })).toBe("1 server, 2 network scans");
    const scans = [item("n", [], { kind: "network_scan" }), item("w", [], { kind: "window" }), item("h", [], { kind: "channel" })];
    expect(grouped(scans, "exact").map((g) => g.kind)).toEqual(["window", "network_scan", "channel"]);
    expect(countsText({})).toBe("nothing");
  });
});
