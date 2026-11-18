import { describe, expect, it } from "vitest";

import { DEVICE_CHECK_TYPES, checkErrors, checkRequests, checkTitle, shapeOf } from "./alerts";
import type { EffectiveSensor } from "./api";

const [ping, tcp, udp] = DEVICE_CHECK_TYPES;

const sensor = (over: Partial<EffectiveSensor>): EffectiveSensor => ({
  origin: "server",
  entry_id: "e1",
  kind: "ping",
  name: "ping",
  instance: "central",
  interval_ms: 60_000,
  timeout_ms: null,
  params: {},
  probe: "central",
  ...over,
});

describe("device checks", () => {
  it("make probe rows with the alert that comes with them", () => {
    const pinged = checkRequests(ping!, { host: "" }, 60_000);
    expect(pinged.sensor).toEqual({ kind: "ping", name: "ping", interval_ms: 60_000, params: {} });
    expect(pinged.threshold).toMatchObject({ metric: "monitorflow_ping_up", comparison: "<", critical: 1, duration_s: 120 });
    const dns = checkRequests(udp!, { port: "53", host: "" }, 60_000);
    expect(dns.sensor).toMatchObject({ kind: "port", name: "udp_53", params: { port: "53", protocol: "udp" } });
    expect(dns.threshold?.metric).toBe("monitorflow_udp_53_up");
    expect(checkRequests(tcp!, { port: "443", host: "nas.lan" }, 60_000).sensor.name).toBe("port_443");
    expect(checkErrors(ping!, { host: "" })).toEqual({});
    expect(Object.keys(checkErrors(udp!, { port: "0", host: "" }))).toEqual(["port"]);
    expect(DEVICE_CHECK_TYPES.every((type) => type.probe)).toBe(true);
  });

  it("say where they run from", () => {
    expect(checkTitle(sensor({}))).toBe("Ping · from central server");
    expect(checkTitle(sensor({ instance: "relay-paris", probe: "relay", params: { host: "198.51.100.7" } }))).toBe(
      "Ping of 198.51.100.7 · from relay-paris",
    );
    expect(checkTitle(sensor({ kind: "port", name: "udp_53", params: { port: "53", protocol: "udp" } }))).toBe(
      "UDP port 53 · from central server",
    );
    expect(checkTitle(sensor({ probe: null, kind: "port", name: "port_22", params: { port: "22" } }))).toBe("Port 22");
    const shape = shapeOf([sensor({})], "monitorflow_ping_up", "central");
    expect(shape).toMatchObject({ type: "event", noun: "the ping", comparison: "<", value: 1 });
  });
});
