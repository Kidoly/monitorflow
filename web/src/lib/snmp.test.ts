import { afterEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { SnmpCheck, SnmpObject } from "@/lib/api";

import { NEW_CHANNEL, NEW_CHECK, channelOf, channelOfRead, formOf, formProblem, formatChannel, requestOf, stateOf, validOid } from "./snmp";

afterEach(() => setActiveLocale("en"));

const check: SnmpCheck = {
  entry_id: "e1",
  name: "snmp_fw",
  instance: "central",
  probe: "relay",
  probe_relay_id: "r1",
  interval_ms: 300_000,
  enabled: true,
  table: null,
  name_column: null,
  channels: [
    {
      oid: "1.3.6.1.4.1.12356.101.4.1.8.0",
      name: "Sessions",
      unit: "sessions",
      limits: { comparison: ">", warning: 5000, critical: 8000, duration_s: 300 },
      key: "sessions",
      metric: "monitorflow_snmp_fw_sessions",
      state_metric: null,
      values: [],
    },
    {
      oid: "1.3.6.1.4.1.6574.1.2.0",
      name: "Temperature",
      divide: 10,
      key: "temperature",
      metric: "monitorflow_snmp_fw_temperature",
      state_metric: null,
      values: [],
    },
    {
      oid: "1.3.6.1.4.1.12356.101.4.1.1.0",
      name: "Firmware",
      type: "text",
      match: "equals",
      expect: "",
      severity: "warning",
      key: "firmware",
      metric: "monitorflow_snmp_fw_firmware",
      state_metric: "monitorflow_snmp_fw_firmware_state",
      values: [],
    },
  ],
};

describe("custom SNMP check forms", () => {
  it("read a stored check back into the same request", () => {
    const form = formOf(check);
    expect(form.probe).toBe("r1");
    expect(form.channels[1]).toMatchObject({ scale: "divide", factor: "10" });
    expect(form.channels[0]).toMatchObject({ comparison: ">", warning: "5000", critical: "8000", durationS: 300 });
    const request = requestOf(form);
    expect(request).toMatchObject({ name: "snmp_fw", probe: "r1", interval_ms: 300_000 });
    expect(request.channels[0]).toEqual({
      oid: "1.3.6.1.4.1.12356.101.4.1.8.0",
      name: "Sessions",
      key: "sessions",
      unit: "sessions",
      limits: { comparison: ">", warning: 5000, critical: 8000, duration_s: 300 },
    });
    expect(request.channels[1]).toMatchObject({ divide: 10, limits: null });
    expect(request.channels[2]).toMatchObject({ type: "text", match: "equals", expect: "", severity: "warning", limits: null });
    expect(request.channels[2]).not.toHaveProperty("divide");
  });

  it("say what is wrong before asking the server", () => {
    expect(formProblem({ ...NEW_CHECK, channels: [{ ...NEW_CHANNEL, oid: "1.3.6.1.2.1.1.3.0", name: "Uptime" }] })).toBeNull();
    const one = (patch: Partial<typeof NEW_CHANNEL>, form: Partial<typeof NEW_CHECK> = {}) =>
      formProblem({ ...NEW_CHECK, ...form, channels: [{ ...NEW_CHANNEL, oid: "1.3.6.1.2.1.1.3.0", name: "Uptime", ...patch }] });
    expect(one({ oid: "sysUpTime" })).toContain("Channel 1");
    expect(one({ name: " " })).toContain("give it a name");
    expect(one({ scale: "divide", factor: "0" })).toContain("above 0");
    expect(one({ comparison: ">", warning: "", critical: "" })).toContain("numbers");
    expect(one({ type: "text", match: "contains", expect: "" })).toContain("expects");
    expect(one({}, { name: "Firewall" })).toContain("lowercase");
    expect(one({}, { mode: "table", table: "x" })).toContain("table");
    expect(one({}, { mode: "table", table: "1.3.6.1.4.1.6574.3.1.1" })).toContain("column");
    setActiveLocale("fr");
    expect(one({ oid: "x" })).toContain("Canal 1");
  });

  it("make channels from the library, MIBs and Browse", () => {
    const raid: SnmpObject = {
      name: "raidStatus",
      module: "TEST-NAS-MIB",
      source: "mib",
      oid: "1.3.6.1.4.1.99999.3.1.1.3",
      kind: "column",
      syntax: "INTEGER",
      enumeration: [[1, "normal"]],
      units: null,
      description: "",
      suggestion: { type: "gauge", unit: "", multiply: 1, divide: 1, lookup: null, table: "1.3.6.1.4.1.99999.3.1.1", name_column: null },
    };
    expect(channelOf(raid)).toMatchObject({ oid: "1.3.6.1.4.1.99999.3.1.1.3", name: "RaidStatus", type: "gauge" });
    const scalar = { ...raid, kind: "scalar" as const, name: "temperature", suggestion: { ...raid.suggestion, divide: 10, table: null } };
    expect(channelOf(scalar)).toMatchObject({ oid: "1.3.6.1.4.1.99999.3.1.1.3.0", scale: "divide", factor: "10" });
    const library = { ...scalar, source: "library" as const, name: "fgSysSesCount.0", oid: "1.3.6.1.4.1.12356.101.4.1.8.0" };
    expect(channelOf(library)).toMatchObject({ oid: "1.3.6.1.4.1.12356.101.4.1.8.0", name: "FgSysSesCount" });
    expect(channelOfRead({ oid: "1.3.6.1.2.1.2.2.1.10.1", name: "ifInOctets.1", type: "Counter32", value: "5" }).type).toBe("counter");
    expect(channelOfRead({ oid: "1.3.6.1.2.1.1.5.0", name: "sysName.0", type: "OctetString", value: "fw" })).toMatchObject({ type: "text", name: "sysName" });
    expect(channelOfRead({ oid: "1.3.6.1.4.1.2021.10.1.3.1", name: null, type: "OctetString", value: "0.15" }).type).toBe("gauge");
  });

  it("show values and states in words", () => {
    expect(formatChannel(23.456, "°C")).toBe("23.5 °C");
    expect(formatChannel(0.156, "")).toBe("0.16");
    expect(formatChannel(1532, "")).toBe("1,532");
    expect(stateOf(2)).toEqual({ tone: "bad", word: "Critical" });
    expect(stateOf(0)?.tone).toBe("ok");
    expect(stateOf(null)).toBeNull();
    expect(validOid(".1.3.6.1")).toBe(true);
    expect(validOid("1.3.x")).toBe(false);
  });
});
