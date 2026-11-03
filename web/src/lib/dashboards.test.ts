import { describe, expect, it } from "vitest";

import type { DashboardSpec, Panel, Server } from "./api";
import {
  READY_MADE,
  copyName,
  copySpec,
  defaultInstance,
  dropMissingServers,
  lowIsBad,
  movePanel,
  newPanel,
  newPanelId,
  placePanel,
  serversWithin,
  sharingLabel,
  suggestedTitle,
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
    expect(sharingLabel({ kind: "customer", id: "1", name: "acme" })).toBe("Customer acme");
    expect(withinOf({ kind: "personal" })).toEqual({ kind: "all" });
    expect(withinOf({ kind: "site", id: "1" })).toEqual({ kind: "site", id: "1" });
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
    for (const d of READY_MADE) {
      const ids = d.spec.panels.map((p) => p.id);
      expect(new Set(ids).size).toBe(ids.length);
      for (const p of d.spec.panels) {
        expect(p.title.length).toBeGreaterThan(0);
        expect(withKind(p, p.kind)).toEqual(p);
      }
    }
  });
});
