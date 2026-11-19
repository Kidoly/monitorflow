import { describe, expect, it } from "vitest";

import type { NetworkScanHost } from "./api";
import { checksWords, failureText, parsePorts, sections, splitTargets, validTarget } from "./discovery";

const host = (address: string, action: NetworkScanHost["proposal"]["action"], isNew = false): NetworkScanHost => ({
  address,
  ping: true,
  rtt_seconds: null,
  ports: [],
  name: null,
  mac: null,
  snmp: false,
  snmp_description: null,
  snmp_name: null,
  snmp_object_id: null,
  snmp_groups: [],
  new: isNew,
  proposal: { action, os: null, monitored_by: null, name: address, template: null, checks: [] },
});

describe("discovery helpers", () => {
  it("split and check addresses as the server takes them", () => {
    expect(splitTargets(" 192.0.2.0/24,\n192.0.2.10-50  198.51.100.7 ")).toEqual([
      "192.0.2.0/24",
      "192.0.2.10-50",
      "198.51.100.7",
    ]);
    for (const ok of ["192.0.2.1", "192.0.2.0/24", "192.0.2.10-50", "192.0.2.10-192.0.2.60", "2001:db8::/120", "[2001:db8::1]"])
      expect(validTarget(ok), ok).toBe(true);
    for (const bad of ["printer.lan", "192.0.2.0/33", "192.0.2.300", "192.0.2.1-999", "http://x", ""])
      expect(validTarget(bad), bad).toBe(false);
  });

  it("read ports", () => {
    expect(parsePorts("443, 22,22 8080")).toEqual([22, 443, 8080]);
    expect(parsePorts("")).toEqual([]);
    expect(parsePorts("0")).toBeNull();
    expect(parsePorts("http")).toBeNull();
  });

  it("word failures and checks", () => {
    const run = { error: "relay_outdated" } as never;
    expect(failureText(run)).toBe("This relay does not run scans yet. Update it, then scan again.");
    expect(failureText({ error: "boom" } as never)).toBe("The scan failed. boom");
    const ups = host("192.0.2.4", "snmp_device");
    ups.proposal.checks = [
      { kind: "ping", name: "ping", port: null },
      { kind: "port", name: "port_80", port: 80 },
      { kind: "snmp", name: "snmp", port: null },
      { kind: "snmp", name: "snmp_ups", port: null },
    ];
    expect(checksWords(ups)).toBe("ping, port 80, SNMP, UPS battery");
  });

  it("put proposals first, new ones on top", () => {
    const { proposed, monitored, dismissed } = sections([
      host("a", "monitored"),
      host("b", "device"),
      host("c", "agent"),
      host("d", "device", true),
      host("e", "dismissed"),
    ]);
    expect(proposed.map((h) => h.address)).toEqual(["c", "d", "b"]);
    expect(monitored.map((h) => h.address)).toEqual(["a"]);
    expect(dismissed.map((h) => h.address)).toEqual(["e"]);
  });
});
