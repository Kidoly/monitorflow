import { describe, expect, it } from "vitest";

import type { FlowView } from "@/lib/api";

import { chartSeries, isFlowPeriod, keyText, parseMbits, periodRange, share } from "./flows";

const view: FlowView = {
  from: "2026-12-26T09:00:00Z",
  to: "2026-12-26T09:02:00Z",
  step_s: 60,
  address: null,
  bytes: 0,
  packets: 0,
  bits_per_second: 0,
  dimensions: [],
  chart: {
    by: "app",
    keys: [{ key: "tcp/443", label: "HTTPS" }],
    points: [
      { at: "2026-12-26T09:00:00Z", values: [1000], other: 0 },
      { at: "2026-12-26T09:01:00Z", values: [2000], other: 50 },
    ],
  },
};

describe("flows", () => {
  it("names keys", () => {
    expect(keyText("app", "tcp/443", "HTTPS")).toBe("HTTPS (tcp/443)");
    expect(keyText("src", "192.0.2.1", null)).toBe("192.0.2.1");
    expect(keyText("in", "3", "Gi0/3")).toBe("Gi0/3 (ifIndex 3)");
    expect(keyText("out", "7", null)).toBe("ifIndex 7");
    expect(keyText("in", "0", null)).toBe("Not said");
  });

  it("stacks the chart's keys and the rest", () => {
    const series = chartSeries(view);
    expect(series.map((s) => s.label)).toEqual(["HTTPS (tcp/443)", "Other"]);
    expect(series[0]?.points[1]).toEqual([Date.parse("2026-12-26T09:01:00Z") / 1000, 2000]);
    expect(series[1]?.points[0]?.[1]).toBe(0);
  });

  it("reads periods and rates", () => {
    expect(isFlowPeriod("7d")).toBe(true);
    expect(isFlowPeriod("2d")).toBe(false);
    const range = periodRange("1h", new Date("2026-12-26T10:00:30Z"));
    expect(range).toEqual({ from: "2026-12-26T09:00:00.000Z", to: "2026-12-26T10:00:00.000Z" });
    expect(parseMbits("12,5")).toBe(12_500_000);
    expect(parseMbits("0")).toBeNull();
    expect(parseMbits("")).toBeNull();
    expect(share(5, 10)).toBe(0.5);
    expect(share(5, 0)).toBe(0);
  });
});
