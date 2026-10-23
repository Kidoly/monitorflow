import { describe, expect, it } from "vitest";

import { ApiError } from "./api";
import { RANGES, chartWindow, metricsError, seriesCsv } from "./charts";

const END = Date.parse("2026-10-19T12:00:00Z") / 1000;

describe("chartWindow", () => {
  const [hour, , day, week, month] = RANGES;
  const now = END + 10;

  it("keeps short ranges raw and long ones rolled up", () => {
    expect(chartWindow(hour, "monitorflow_cpu_usage_ratio", 600, now)).toEqual({ start: END - 3600, end: END, step: 30 });
    expect(chartWindow(day, "monitorflow_cpu_usage_ratio", 60, now)).toMatchObject({ step: 300, rollup: "max" });
    expect(chartWindow(week, "monitorflow_port_up", 60, now)).toMatchObject({ step: 1800, rollup: "min" });
    expect(chartWindow(month, "monitorflow_disk_total_bytes", 60, now)).toMatchObject({ step: 7200, rollup: "avg" });
  });

  it("rounds a long interval up to a multiple of the range's step", () => {
    const window = chartWindow(day, "monitorflow_cpu_usage_ratio", 420, now);
    expect(window.step).toBe(600);
    expect(window.end % 600).toBe(0);
    expect(window.end - window.start).toBe(86_400);
  });

  it("keeps every range within the proxy's point limit and the API's 31 days", () => {
    for (const range of RANGES) {
      expect(range.seconds / range.step).toBeLessThanOrEqual(500);
      expect(range.seconds).toBeLessThanOrEqual(31 * 86_400);
    }
  });
});

describe("metricsError", () => {
  it("explains the metrics backend's failures", () => {
    expect(metricsError(new ApiError(502, "bad gateway"))).toMatch(/backend is unavailable/);
    expect(metricsError(new ApiError(504, "timeout"))).toMatch(/Try a shorter range/);
    expect(metricsError(new ApiError(403, "forbidden"))).toMatch(/do not have access/);
  });
});

describe("seriesCsv", () => {
  it("writes one column per series, aligned by time, with gaps empty", () => {
    const csv = seriesCsv([
      { label: "/var", points: [[END, 0.5], [END + 60, null]] },
      { label: 'a "b", c', points: [[END + 60, 2]] },
    ]);
    expect(csv).toBe(
      'time,/var,"a ""b"", c"\n2026-10-19T12:00:00.000Z,0.5,\n2026-10-19T12:01:00.000Z,,2\n',
    );
  });
});
