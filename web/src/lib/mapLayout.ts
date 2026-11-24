import type { MapLink, MapNode } from "./api";

/**
 * Lays out a network map (ADR-0185), with no library: a block per site,
 * side by side, each drawn top-down from what has nothing above it, the
 * central server above them all. Parents of relay and dependency links
 * sit above their children, and a switch or router above what it sees;
 * nodes with no link are a grid under their site's tree. Deterministic:
 * the same map always lays out the same, on a screen as in the editor.
 */

export interface Point {
  x: number;
  y: number;
}

/** Room for a node and its name, across and down. */
export const GAP_X = 170;
export const GAP_Y = 150;
/** Between the blocks of two sites. */
const BLOCK_GAP = 140;
/** Nodes with no link, per row of their grid. */
const GRID_COLUMNS = 8;

const NETWORK = new Set(["switch", "router", "firewall"]);

/** Whether a node passes traffic for others: what a neighbour link hangs under. */
function isNetwork(node: MapNode): boolean {
  return node.kind === "relay" || node.kind === "central" || (node.template !== null && NETWORK.has(node.template));
}

/** `parent → child` of a link, or null when it says nothing of up and down. */
function direction(link: MapLink, nodes: Map<string, MapNode>): [string, string] | null {
  if (link.source === "relay" || link.source === "dependency") return [link.from, link.to];
  const from = nodes.get(link.from);
  const to = nodes.get(link.to);
  if (!from || !to) return null;
  if (isNetwork(from) && !isNetwork(to)) return [link.from, link.to];
  if (isNetwork(to) && !isNetwork(from)) return [link.to, link.from];
  return null;
}

function byName(a: MapNode, b: MapNode): number {
  return a.name.localeCompare(b.name) || a.key.localeCompare(b.key);
}

/** Positions of `nodes` in one block, from (0, 0) down and right; its width. */
function layBlock(nodes: MapNode[], links: MapLink[], all: Map<string, MapNode>): { at: Map<string, Point>; width: number } {
  const inBlock = new Set(nodes.map((n) => n.key));
  const children = new Map<string, string[]>();
  const parents = new Map<string, string[]>();
  const neighbours = new Map<string, Set<string>>();
  for (const link of links) {
    if (!inBlock.has(link.from) || !inBlock.has(link.to)) continue;
    for (const [a, b] of [
      [link.from, link.to],
      [link.to, link.from],
    ] as const) {
      if (!neighbours.has(a)) neighbours.set(a, new Set());
      neighbours.get(a)!.add(b);
    }
    const way = direction(link, all);
    if (!way) continue;
    const [parent, child] = way;
    children.set(parent, [...(children.get(parent) ?? []), child]);
    parents.set(child, [...(parents.get(child) ?? []), parent]);
  }
  const linked = nodes.filter((n) => neighbours.has(n.key)).sort(byName);
  const loose = nodes.filter((n) => !neighbours.has(n.key)).sort(byName);

  // Layers by the longest path from the top, at most one pass per node so
  // a loop of dependencies cannot hang the page.
  const layer = new Map<string, number>(linked.map((n) => [n.key, 0]));
  for (let pass = 0; pass < linked.length; pass++) {
    let moved = false;
    for (const node of linked) {
      for (const parent of parents.get(node.key) ?? []) {
        const wanted = (layer.get(parent) ?? 0) + 1;
        if (wanted > (layer.get(node.key) ?? 0) && wanted < linked.length + 1) {
          layer.set(node.key, wanted);
          moved = true;
        }
      }
    }
    if (!moved) break;
  }
  const layers: string[][] = [];
  for (const node of linked) {
    const index = layer.get(node.key) ?? 0;
    (layers[index] ??= []).push(node.key);
  }
  const rows = layers.filter((row) => row && row.length > 0);

  // Fewer crossings: each row in the order of its neighbours above it.
  const order = new Map<string, number>();
  rows.forEach((row) => row.forEach((key, i) => order.set(key, i)));
  for (let sweep = 0; sweep < 3; sweep++) {
    for (let r = 1; r < rows.length; r++) {
      const above = new Set(rows[r - 1]);
      const centre = (key: string) => {
        const seen = [...(neighbours.get(key) ?? [])].filter((k) => above.has(k)).map((k) => order.get(k) ?? 0);
        return seen.length ? seen.reduce((a, b) => a + b, 0) / seen.length : (order.get(key) ?? 0);
      };
      const row = rows[r]!;
      const centres = new Map(row.map((k) => [k, centre(k)]));
      row.sort((a, b) => centres.get(a)! - centres.get(b)! || a.localeCompare(b));
      row.forEach((key, i) => order.set(key, i));
    }
  }

  const gridRows = Math.ceil(loose.length / GRID_COLUMNS);
  const widest = Math.max(1, ...rows.map((row) => row.length), Math.min(loose.length, GRID_COLUMNS));
  const width = (widest - 1) * GAP_X;
  const at = new Map<string, Point>();
  rows.forEach((row, r) => {
    const offset = (width - (row.length - 1) * GAP_X) / 2;
    row.forEach((key, i) => at.set(key, { x: offset + i * GAP_X, y: r * GAP_Y }));
  });
  const top = rows.length * GAP_Y;
  for (let g = 0; g < gridRows; g++) {
    const row = loose.slice(g * GRID_COLUMNS, (g + 1) * GRID_COLUMNS);
    const offset = (width - (row.length - 1) * GAP_X) / 2;
    row.forEach((node, i) => at.set(node.key, { x: offset + i * GAP_X, y: top + g * GAP_Y }));
  }
  return { at, width };
}

/** The site whose block a node goes in: its own, else that of the first node it links to. */
function blockOf(node: MapNode, links: MapLink[], nodes: Map<string, MapNode>): string {
  if (node.site_id) return node.site_id;
  for (const link of links) {
    const other = link.from === node.key ? link.to : link.to === node.key ? link.from : null;
    const site = other ? nodes.get(other)?.site_id : null;
    if (site) return site;
  }
  return "";
}

/** Positions of every node of `nodes`, laid out from scratch. */
export function autoLayout(nodes: MapNode[], links: MapLink[]): Map<string, Point> {
  const all = new Map(nodes.map((n) => [n.key, n]));
  const central = nodes.find((n) => n.kind === "central");
  const blocks = new Map<string, MapNode[]>();
  for (const node of nodes) {
    if (node === central) continue;
    const block = blockOf(node, links, all);
    blocks.set(block, [...(blocks.get(block) ?? []), node]);
  }
  const at = new Map<string, Point>();
  let left = 0;
  const top = central ? GAP_Y : 0;
  for (const block of [...blocks.keys()].sort()) {
    const laid = layBlock(blocks.get(block)!, links, all);
    for (const [key, point] of laid.at) at.set(key, { x: point.x + left, y: point.y + top });
    left += laid.width + GAP_X + BLOCK_GAP;
  }
  if (central) {
    const width = Math.max(0, left - GAP_X - BLOCK_GAP);
    at.set(central.key, { x: width / 2, y: 0 });
  }
  return at;
}

/**
 * Where each visible node is drawn: where people placed it, and the others
 * laid out, under what was placed when some were.
 */
export function positions(nodes: MapNode[], links: MapLink[]): Map<string, Point> {
  const shown = nodes.filter((n) => !n.hidden);
  const placed = shown.filter((n) => n.x !== null && n.y !== null);
  const unplaced = shown.filter((n) => n.x === null || n.y === null);
  const at = new Map<string, Point>(placed.map((n) => [n.key, { x: n.x!, y: n.y! }]));
  if (unplaced.length === 0) return at;
  const laid = autoLayout(unplaced, links);
  const below = placed.length ? Math.max(...placed.map((n) => n.y!)) + GAP_Y * 1.5 : 0;
  const left = placed.length ? Math.min(...placed.map((n) => n.x!)) : 0;
  for (const [key, point] of laid) at.set(key, { x: point.x + left, y: point.y + below });
  return at;
}

export interface Box {
  x: number;
  y: number;
  width: number;
  height: number;
}

/** The box around `points`, with `pad` all around; null without points. */
export function bounds(points: Point[], pad: number): Box | null {
  if (points.length === 0) return null;
  const xs = points.map((p) => p.x);
  const ys = points.map((p) => p.y);
  const x = Math.min(...xs) - pad;
  const y = Math.min(...ys) - pad;
  return { x, y, width: Math.max(...xs) + pad - x, height: Math.max(...ys) + pad - y };
}

/** The union of `boxes`; null when there are none. */
export function union(boxes: (Box | null | undefined)[]): Box | null {
  const present = boxes.filter((b): b is Box => !!b);
  if (present.length === 0) return null;
  const x = Math.min(...present.map((b) => b.x));
  const y = Math.min(...present.map((b) => b.y));
  const right = Math.max(...present.map((b) => b.x + b.width));
  const bottom = Math.max(...present.map((b) => b.y + b.height));
  return { x, y, width: right - x, height: bottom - y };
}
