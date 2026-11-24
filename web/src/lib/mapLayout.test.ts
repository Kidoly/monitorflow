import { describe, expect, it } from "vitest";

import type { MapLink, MapNode } from "./api";
import { GAP_X, GAP_Y, autoLayout, bounds, positions } from "./mapLayout";

function node(key: string, extra: Partial<MapNode> = {}): MapNode {
  return {
    key,
    kind: "server",
    name: key,
    site_id: "paris",
    state: "ok",
    server_id: key,
    relay_id: null,
    address: null,
    template: null,
    os_family: "debian",
    x: null,
    y: null,
    hidden: false,
    ...extra,
  };
}

function link(from: string, to: string, source: MapLink["source"]): MapLink {
  return { id: `${from}|${to}`, from, to, source, state: "ok", label: null, from_port: null, to_port: null, interface: null, traffic: null };
}

describe("autoLayout", () => {
  const nodes = [
    node("central", { kind: "central", site_id: null }),
    node("gw", { kind: "relay" }),
    node("sw", { kind: "device", template: "switch" }),
    node("web"),
    node("db"),
    node("lonely"),
    node("lyon-1", { site_id: "lyon" }),
  ];
  const links = [
    link("central", "gw", "relay"),
    link("gw", "web", "relay"),
    link("sw", "db", "neighbor"),
    link("db", "sw", "neighbor"),
    link("sw", "web", "dependency"),
  ];

  it("puts parents above children and the central server above everything", () => {
    const at = autoLayout(nodes, links);
    expect(at.size).toBe(nodes.length);
    expect(at.get("central")!.y).toBe(0);
    expect(at.get("gw")!.y).toBeLessThan(at.get("web")!.y);
    expect(at.get("sw")!.y).toBeLessThan(at.get("web")!.y);
    // A switch sits above what it sees, whichever way the neighbour said it.
    expect(at.get("sw")!.y).toBeLessThan(at.get("db")!.y);
    // What has no link goes in a grid under its site's tree.
    expect(at.get("lonely")!.y).toBeGreaterThan(at.get("web")!.y);
  });

  it("keeps each site in a block of its own, and lays out the same every time", () => {
    const at = autoLayout(nodes, links);
    const paris = ["gw", "sw", "web", "db", "lonely"].map((k) => at.get(k)!.x);
    const lyon = at.get("lyon-1")!.x;
    expect(lyon < Math.min(...paris) - GAP_X || lyon > Math.max(...paris) + GAP_X).toBe(true);
    expect(autoLayout([...nodes].reverse(), links)).toEqual(at);
  });

  it("survives a loop of dependencies", () => {
    const loop = [node("a"), node("b")];
    const at = autoLayout(loop, [link("a", "b", "dependency"), link("b", "a", "dependency")]);
    expect(at.size).toBe(2);
    for (const p of at.values()) expect(Number.isFinite(p.y)).toBe(true);
  });
});

describe("positions", () => {
  it("keeps placed nodes, lays out the others under them, and leaves hidden ones off", () => {
    const at = positions(
      [node("a", { x: 500, y: 40 }), node("b"), node("c", { hidden: true, x: 0, y: 0 })],
      [],
    );
    expect(at.get("a")).toEqual({ x: 500, y: 40 });
    expect(at.get("b")!.y).toBeGreaterThanOrEqual(40 + GAP_Y);
    expect(at.has("c")).toBe(false);
  });

  it("boxes points with room around them", () => {
    expect(bounds([], 10)).toBeNull();
    expect(bounds([{ x: 0, y: 0 }, { x: 100, y: 50 }], 10)).toEqual({ x: -10, y: -10, width: 120, height: 70 });
  });
});
