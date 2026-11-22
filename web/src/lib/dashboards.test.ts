import { describe, expect, it } from "vitest";

import type { DashboardSpec, Panel, Server, Sharing } from "./api";
import {
  KINDS_FOR,
  baseKind,
  defaultKind,
  sourceOf,
  withSource,
  copyName,
  copySpec,
  defaultInstance,
  dropMissingServers,
  levelOf,
  lowIsBad,
  movePanel,
  newPanel,
  newPanelId,
  panelFormat,
  panelScale,
  placePanel,
  readyMades,
  serversWithin,
  sharingLabel,
  suggestedTitle,
  thresholdBands,
  withKind,
  withinOf,
} from "./dashboards";

const chart: Panel = {
  id: "a",
  title: "CPU",
  kind: "chart",
  width: 2,
  servers: { kind: "all" },
  metric: "monitorflow_cpu_usage_ratio",
  instance: "total",
  combine: "each",
};

const spec = (panels: Panel[]): DashboardSpec => ({ version: 1, range: "6h", panels });

describe("panels", () => {
  it("change kind keeping what still applies", () => {
    expect(withKind(chart, "figure")).toEqual({ ...chart, kind: "figure", combine: "avg" });
    expect(withKind({ ...chart, combine: "max" }, "figure").combine).toBe("max");
    expect(withKind({ ...chart, combine: "count" }, "chart").combine).toBe("each");
    const { combine: _c, ...rest } = chart;
    void _c;
    expect(withKind(chart, "top")).toEqual({ ...rest, kind: "top", order: "highest", count: 5 });
    expect(withKind({ ...chart, metric: "monitorflow_disk_available_bytes" }, "top").order).toBe("lowest");
    expect(withKind(chart, "alerts")).toEqual({ id: "a", title: "CPU", kind: "alerts", width: 2, servers: { kind: "all" }, count: 10 });
    // Back from alerts: CPU again, nothing left over.
    expect(withKind(withKind(chart, "alerts"), "chart")).toEqual({
      id: "a",
      title: "CPU",
      kind: "chart",
      width: 2,
      servers: { kind: "all" },
      metric: "monitorflow_cpu_usage_ratio",
      combine: "each",
    });
  });

  it("get titles from their choices", () => {
    expect(suggestedTitle(chart)).toBe("CPU usage");
    expect(suggestedTitle({ ...chart, combine: "avg" })).toBe("CPU usage, average");
    expect(suggestedTitle({ ...chart, kind: "figure", combine: "count" })).toBe("Servers reporting cpu usage");
    expect(suggestedTitle({ ...chart, kind: "top", order: "lowest", metric: "monitorflow_disk_used_ratio", instance: "/var" })).toBe(
      "Lowest: Disk used /var",
    );
    expect(suggestedTitle({ ...chart, kind: "alerts" })).toBe("Open alerts");
    expect(newPanel().title).toBe("CPU usage");
  });

  it("know when low is bad and default to the total", () => {
    expect(lowIsBad("monitorflow_memory_available_ratio")).toBe(true);
    expect(lowIsBad("monitorflow_port_up")).toBe(true);
    expect(lowIsBad("monitorflow_disk_used_ratio")).toBe(false);
    expect(defaultInstance(["cpu0", "total"])).toBe("total");
    expect(defaultInstance(["/", "/var"])).toBeUndefined();
  });

  it("get ids of their own", () => {
    const id = newPanelId(["a"]);
    expect(id).toMatch(/^[a-z0-9]{1,8}$/);
    expect(id).not.toBe("a");
  });

  it("move within bounds", () => {
    expect(movePanel([1, 2, 3], 0, 1)).toEqual([2, 1, 3]);
    expect(movePanel([1, 2, 3], 2, 1)).toEqual([1, 2, 3]);
    expect(movePanel([1, 2, 3], 0, -1)).toEqual([1, 2, 3]);
    expect(placePanel([1, 2, 3, 4], 3, 0)).toEqual([4, 1, 2, 3]);
    expect(placePanel([1, 2, 3, 4], 0, 2)).toEqual([2, 3, 1, 4]);
  });
});

describe("dashboards", () => {
  it("lose servers that are gone, falling back to every server", () => {
    const named = (ids: string[]): Panel => ({ ...chart, servers: { kind: "list", ids } });
    const { spec: cleaned, dropped } = dropMissingServers(spec([named(["s1", "s2"]), named(["s3"]), chart]), new Set(["s1"]));
    expect(dropped).toBe(2);
    expect(cleaned.panels.map((p) => p.servers)).toEqual([{ kind: "list", ids: ["s1"] }, { kind: "all" }, { kind: "all" }]);
    const same = spec([named(["s1"])]);
    expect(dropMissingServers(same, new Set(["s1"])).spec.panels[0]).toBe(same.panels[0]);
  });

  it("are copied with the servers they showed", () => {
    const original = spec([chart, { ...chart, id: "b", servers: { kind: "list", ids: ["s1"] } }]);
    expect(copySpec(original, { kind: "all" })).toBe(original);
    expect(copySpec(original, { kind: "site", id: "site-1" }).panels.map((p) => p.servers)).toEqual([
      { kind: "site", id: "site-1" },
      { kind: "list", ids: ["s1"] },
    ]);
    expect(copyName("x".repeat(100))).toHaveLength(80);
    expect(copyName("Ops")).toBe("Copy of Ops");
  });

  it("say who they are shared with in words", () => {
    expect(sharingLabel({ kind: "personal" })).toBe("Only me");
    expect(sharingLabel({ kind: "site", id: "1", name: "paris" })).toBe("Site paris");
    expect(sharingLabel({ kind: "customer", id: "1", name: "acme" }, false)).toBe("All sites");
    expect(sharingLabel({ kind: "customer", id: "1", name: "acme" })).toBe("Customer acme");
    expect(withinOf({ kind: "personal" })).toEqual({ kind: "all" });
    expect(withinOf({ kind: "site", id: "1" })).toEqual({ kind: "site", id: "1" });
    expect(withinOf({ kind: "customer", id: "1", name: "acme" } as Sharing)).toEqual({ kind: "customer", id: "1" });
  });

  it("offer the servers of their sharing, without removed ones", () => {
    const server = (id: string, site: string, customer: string, revoked = false) =>
      ({ id, site_id: site, customer_id: customer, revoked_at: revoked ? "2026-01-01T00:00:00Z" : null }) as Server;
    const servers = [server("a", "s1", "c1"), server("b", "s2", "c1"), server("c", "s3", "c2"), server("d", "s1", "c1", true)];
    expect(serversWithin(servers, { kind: "all" }).map((s) => s.id)).toEqual(["a", "b", "c"]);
    expect(serversWithin(servers, { kind: "site", id: "s1" }).map((s) => s.id)).toEqual(["a"]);
    expect(serversWithin(servers, { kind: "customer", id: "c1" }).map((s) => s.id)).toEqual(["a", "b"]);
  });

  it("come ready-made with valid panels", () => {
    for (const d of readyMades()) {
      const ids = d.spec.panels.map((p) => p.id);
      expect(new Set(ids).size).toBe(ids.length);
      for (const p of d.spec.panels) {
        expect(p.title.length).toBeGreaterThan(0);
        expect(withKind(p, p.kind)).toEqual(p);
      }
    }
  });
});

describe("panel kinds and display options", () => {
  it("draws the data of a base kind", () => {
    expect(["area", "columns", "gauge", "bars", "chart", "alerts"].map((k) => baseKind(k as Panel["kind"]))).toEqual([
      "chart",
      "chart",
      "figure",
      "top",
      "chart",
      "alerts",
    ]);
  });

  it("keeps the display options when the kind changes, a legend only over time", () => {
    const withOptions: Panel = { ...chart, legend: false, thresholds: { warning: 0.8 }, color: 2, unit: "ratio" };
    const gauge = withKind(withOptions, "gauge");
    expect(gauge).toMatchObject({ kind: "gauge", combine: "max", thresholds: { warning: 0.8 }, color: 2, unit: "ratio" });
    expect(gauge.legend).toBeUndefined();
    expect(withKind(withOptions, "area")).toMatchObject({ kind: "area", combine: "each", legend: false });
    expect(withKind(withOptions, "bars")).toMatchObject({ kind: "bars", order: "highest", count: 5 });
    expect(withKind(withOptions, "heatmap")).toMatchObject({ kind: "heatmap", metric: chart.metric, count: 10, color: 2 });
    expect(withKind(withOptions, "table")).toMatchObject({ kind: "table", columns: [{ metric: chart.metric, instance: "total" }], order: "highest", count: 20 });
    const text = withKind({ ...withOptions, servers: { kind: "site", id: "x" } }, "text");
    expect(text).toMatchObject({ kind: "text", servers: { kind: "all" } });
    expect(text.text).toBeTruthy();
    expect(text.metric).toBeUndefined();
    const alerts = withKind(withOptions, "alerts");
    expect(alerts.thresholds).toBeUndefined();
    expect(alerts.color).toBeUndefined();
  });

  it("reads thresholds upward, or downward where low is bad", () => {
    const cpu: Panel = { ...chart, thresholds: { warning: 0.8, critical: 0.9 } };
    expect([0.5, 0.8, 0.95, null].map((v) => levelOf(cpu, v))).toEqual(["ok", "warning", "critical", null]);
    expect(levelOf(chart, 0.99)).toBeNull();
    const free: Panel = { ...chart, metric: "monitorflow_memory_available_ratio", thresholds: { warning: 0.2, critical: 0.1 } };
    expect([0.5, 0.15, 0.05].map((v) => levelOf(free, v))).toEqual(["ok", "warning", "critical"]);
    expect(thresholdBands(free, 0, 1)).toEqual([
      { from: 0, to: 0.1, color: "var(--threshold-crit)" },
      { from: 0.1, to: 0.2, color: "var(--threshold-warn)" },
    ]);
    expect(thresholdBands(cpu, 0, 1)).toEqual([
      { from: 0.8, to: 0.9, color: "var(--threshold-warn)" },
      { from: 0.9, to: 1, color: "var(--threshold-crit)" },
    ]);
  });

  it("scales ratios from 0 to 100 %, the rest from 0 to the highest value", () => {
    expect(panelScale(chart, [0.4])).toEqual({ min: 0, max: 1 });
    const bytes: Panel = { ...chart, metric: "monitorflow_memory_used_bytes" };
    expect(panelScale(bytes, [10, 40, null])).toEqual({ min: 0, max: 40 });
    expect(panelScale({ ...bytes, thresholds: { critical: 50 } }, [10])).toEqual({ min: 0, max: 50 });
    expect(panelScale({ ...bytes, min: 5, max: 20 }, [40])).toEqual({ min: 5, max: 20 });
  });

  it("writes values in the panel's unit, a count as a number", () => {
    expect(panelFormat(chart)(0.5)).toBe("50%");
    expect(panelFormat({ ...chart, unit: "bytes", metric: "monitorflow_x_total" })(2048)).toBe("2 KiB");
    expect(panelFormat({ ...chart, kind: "figure", combine: "count" })(12)).toBe("12");
  });
});

const CPU_METRIC = "monitorflow_cpu_usage_ratio";

describe("what a panel shows", () => {
  it("reads the source of each kind", () => {
    const of = (fields: Partial<Panel>) => sourceOf({ ...chart, ...fields });
    expect([of({}), of({ kind: "table" }), of({ kind: "timeline" }), of({ kind: "text" })]).toEqual(["metric", "metric", "alerts", "text"]);
    expect([of({ kind: "states", metric: undefined }), of({ kind: "pie" })]).toEqual(["states", "metric"]);
    for (const [source, kinds] of Object.entries(KINDS_FOR)) {
      for (const kind of kinds) expect(sourceOf(withSource(withKind(chart, kind), source as keyof typeof KINDS_FOR))).toBe(source);
    }
  });

  it("switches source, keeping how a metric is drawn", () => {
    const area = withKind(chart, "area");
    const disk = withSource(area, "metric", "monitorflow_disk_used_ratio");
    expect(disk).toMatchObject({ kind: "area", metric: "monitorflow_disk_used_ratio" });
    expect(disk.instance).toBeUndefined();
    const states = withSource(area, "states");
    expect(states).toMatchObject({ kind: "states" });
    expect(states.metric).toBeUndefined();
    expect(withSource(states, "metric", "monitorflow_ping_up").kind).toBe("states");
    expect(withSource(states, "metric", CPU_METRIC)).toMatchObject({ kind: "chart", instance: "total", combine: "each" });
    expect(withSource(chart, "alerts")).toMatchObject({ kind: "alerts", count: 10 });
    expect(defaultKind("monitorflow_http_shop_up")).toBe("states");
  });
});
