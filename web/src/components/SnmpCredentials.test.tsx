import { describe, expect, it } from "vitest";

import { formatValue, unitOf } from "@/lib/format";
import { formatSensorValue, primaryMetric, seriesTitle } from "@/lib/sensors";
import type { EffectiveSensor } from "@/lib/api";

import { fromStored, snmpProblem, snmpRequest, snmpSummary } from "./SnmpCredentials";

describe("SNMP credentials", () => {
  it("are checked before they are sent", () => {
    expect(snmpProblem({ version: "v2c" }, false)).toBe("Enter the community.");
    expect(snmpProblem({ version: "v2c" }, true)).toBeNull();
    expect(snmpProblem({ version: "v3", user: " " }, false)).toBe("Enter the user.");
    expect(snmpProblem({ version: "v3", user: "u", auth_protocol: "sha256", auth_password: "short" }, false)).toBe("At least 8 characters.");
    expect(snmpProblem({ version: "v3", user: "u", auth_protocol: "sha256" }, true)).toBeNull();
    expect(snmpProblem({ version: "v3", user: "u", auth_protocol: "sha256", auth_password: "long enough" }, false)).toBeNull();
  });

  it("send what their version uses, empty secrets left out", () => {
    expect(snmpRequest({ version: "v2c", community: "c", user: "x" })).toEqual({ version: "v2c", port: 161, community: "c" });
    expect(snmpRequest({ version: "v3", user: " u ", auth_protocol: "sha", auth_password: "", priv_protocol: "aes128", priv_password: "p".repeat(8) })).toEqual({
      version: "v3",
      port: 161,
      user: "u",
      auth_protocol: "sha",
      priv_protocol: "aes128",
      priv_password: "pppppppp",
    });
    const stored = {
      version: "v3" as const, port: 1161, user: "u", auth_protocol: "sha256", priv_protocol: null,
      community_set: false, auth_password_set: true, priv_password_set: false, updated_at: "2026-11-18T10:00:00Z",
    };
    expect(fromStored(stored)).toEqual({ version: "v3", port: 1161, user: "u", auth_protocol: "sha256" });
    expect(snmpSummary(stored)).toBe("v3, user u, port 1161");
    expect(snmpSummary(null)).toBe("Not set");
  });
});

describe("SNMP values", () => {
  const sensor = (what: string): EffectiveSensor => ({
    origin: "server", entry_id: "e", kind: "snmp", name: `snmp_${what}`, instance: "central", interval_ms: 60_000,
    timeout_ms: null, params: { what }, probe: "central",
  });

  it("stand for their group, interfaces by their own names", () => {
    expect(primaryMetric(sensor("if"))).toEqual({ metric: "monitorflow_snmp_if_up", instance: "central" });
    expect(primaryMetric({ ...sensor("x"), params: { what: "interfaces" } })).toEqual({ metric: "monitorflow_snmp_x_oper_up" });
    expect(primaryMetric({ ...sensor("ups"), params: { what: "ups" } })).toEqual({ metric: "monitorflow_snmp_ups_battery_charge_ratio", instance: "central" });
    const ifs = { ...sensor("if"), params: { what: "interfaces" } };
    expect(seriesTitle(ifs, "monitorflow_snmp_if_oper_up", "Gi0/1")).toBe("Interface up Gi0/1");
    expect(formatSensorValue("monitorflow_snmp_if_oper_up", 0)).toBe("down");
    expect(formatSensorValue("monitorflow_snmp_ups_on_battery", 1)).toBe("yes");
    expect(formatSensorValue("monitorflow_snmp_up", 1)).toBe("answers");
  });

  it("read in their units", () => {
    expect(formatValue(42, unitOf("monitorflow_snmp_temp_celsius"))).toBe("42 °C");
    expect(formatValue(1e9, unitOf("monitorflow_snmp_if_speed_bits_per_second"))).toBe("1 Gbit/s");
    expect(formatValue(100e6, unitOf("monitorflow_snmp_if_speed_bits_per_second"))).toBe("100 Mbit/s");
    expect(formatValue(125_000, unitOf("monitorflow_snmp_if_in_bytes_per_second"))).toMatch(/KiB\/s$/);
  });
});
