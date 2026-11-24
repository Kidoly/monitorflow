import { describe, expect, it } from "vitest";

import type { MapLink, MapLive } from "./api";
import { emptyDocument, linkWidth, newId, nodeHref, place, setHidden, trafficText, withDocument } from "./maps";

const link = (extra: Partial<MapLink> = {}): MapLink => ({
  id: "l",
  from: "a",
  to: "b",
  source: "neighbor",
  state: "ok",
  label: null,
  from_port: null,
  to_port: null,
  interface: null,
  traffic: null,
  ...extra,
});

describe("maps", () => {
  it("writes traffic in bits and thickens busy links", () => {
    const busy = link({ traffic: { in_bytes_per_second: 1_250_000, out_bytes_per_second: 125_000, utilization: 0.5, up: true } });
    expect(trafficText(busy)).toMatch(/^↓ 10.*↑ 1/);
    expect(linkWidth(busy)).toBe(5);
    expect(linkWidth(link())).toBe(2);
    expect(trafficText(link())).toBeNull();
    expect(trafficText(link({ traffic: { in_bytes_per_second: null, out_bytes_per_second: null, utilization: null, up: false } }))).toBe(
      "Interface down",
    );
  });

  it("places, hides and shows nodes in the document", () => {
    let d = place(emptyDocument(), "server:1", 10.04, 20);
    expect(d.nodes).toEqual([{ key: "server:1", x: 10, y: 20 }]);
    d = setHidden(d, "server:1", true, { x: 0, y: 0 });
    expect(d.nodes).toEqual([{ key: "server:1", x: 10, y: 20, hidden: true }]);
    // Moving it keeps it hidden; showing it again keeps where it was.
    d = place(d, "server:1", 30, 40);
    expect(d.nodes[0]!.hidden).toBe(true);
    d = setHidden(d, "server:1", false, { x: 0, y: 0 });
    expect(d.nodes).toEqual([{ key: "server:1", x: 30, y: 40 }]);
    d = setHidden(d, "central", true, { x: 5, y: 6 });
    expect(d.nodes[1]).toEqual({ key: "central", x: 5, y: 6, hidden: true });
  });

  it("names new zones, links and labels apart", () => {
    const d = { ...emptyDocument(), zones: [{ id: "zone1", label: "", x: 0, y: 0, width: 1, height: 1, color: 1 }] };
    expect(newId(d, "zone")).toBe("zone2");
    expect(newId(d, "label")).toBe("label1");
  });

  it("draws the draft over what the map shows now", () => {
    const live: MapLive = {
      id: "m",
      name: "Paris",
      group_by_site: true,
      sites: [],
      nodes: [
        { key: "a", kind: "server", name: "a", site_id: null, state: "ok", server_id: "a", relay_id: null, address: null, template: null, os_family: null, x: 1, y: 1, hidden: false },
        { key: "b", kind: "relay", name: "b", site_id: null, state: "offline", server_id: null, relay_id: "b", address: null, template: null, os_family: null, x: null, y: null, hidden: false },
      ],
      links: [link({ source: "relay" }), link({ id: "gone", source: "manual" })],
      zones: [],
      labels: [],
      background: null,
      neighbors_read_at: null,
      truncated: false,
      now: 0,
    };
    const draft = { ...emptyDocument(), nodes: [{ key: "b", x: 7, y: 8, hidden: true }], links: [{ id: "new", from: "a", to: "b", label: "Fibre" }] };
    const shown = withDocument(live, draft);
    expect(shown.nodes.map((n) => [n.x, n.y, n.hidden])).toEqual([
      [null, null, false],
      [7, 8, true],
    ]);
    expect(shown.links.map((l) => [l.source, l.id, l.label])).toEqual([
      ["relay", "l", null],
      ["manual", "new", "Fibre"],
    ]);
    expect(nodeHref(live.nodes[0]!)).toBe("/servers/view/?id=a");
    expect(nodeHref(live.nodes[1]!)).toMatch(/b$/);
  });
});
