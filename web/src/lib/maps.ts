import { t } from "@/i18n";

import type { MapDocument, MapLink, MapLinkState, MapLive, MapNode, MapNodeState, MapPlacement } from "./api";
import { stateColor } from "./dashboards";
import { formatValue } from "./format";
import { relayHref, serverHref } from "./routes";

/** A map refreshes as dashboards do (ADR-0095). */
export const MAP_REFRESH_MS = 30_000;
/** Largest background image the API takes. */
export const MAX_BACKGROUND_BYTES = 1 << 20;

/** An empty map: every source but scans, zones per site. */
export function emptyDocument(): MapDocument {
  return {
    version: 1,
    sources: { relays: true, dependencies: true, neighbors: true, discovered: false },
    group_by_site: true,
    nodes: [],
    zones: [],
    links: [],
    labels: [],
  };
}

/** A node's colour: its state as dashboards colour it, grey when nothing watches it. */
export function nodeColor(state: MapNodeState): string {
  return state === "unknown" ? "var(--border-strong)" : stateColor(state);
}

/** A link's colour: red and amber loud, a working link quiet. */
export function linkColor(state: MapLinkState): string {
  switch (state) {
    case "critical":
      return "var(--threshold-crit)";
    case "warning":
      return "var(--threshold-warn)";
    case "ok":
      return "var(--series-3)";
    default:
      return "var(--border-strong)";
  }
}

/** A link's width: thicker the busier it is, from 2 to 8. */
export function linkWidth(link: MapLink): number {
  const use = link.traffic?.utilization;
  return use === null || use === undefined ? 2 : 2 + 6 * Math.min(1, Math.max(0, use));
}

/** "↓ 10 Mb/s ↑ 2.5 Mb/s" for a link with traffic; null without. */
export function trafficText(link: MapLink): string | null {
  const traffic = link.traffic;
  if (!traffic) return null;
  const bits = (bytes: number | null) => (bytes === null ? null : formatValue(bytes * 8, "bits_per_second"));
  const parts = [
    bits(traffic.in_bytes_per_second) && `↓ ${bits(traffic.in_bytes_per_second)}`,
    bits(traffic.out_bytes_per_second) && `↑ ${bits(traffic.out_bytes_per_second)}`,
  ].filter(Boolean);
  if (traffic.up === false) return t("maps.link.down");
  return parts.length ? parts.join(" ") : null;
}

/** Where a click on the node goes: its page; null for what has none. */
export function nodeHref(node: MapNode): string | null {
  if (node.server_id) return serverHref(node.server_id);
  if (node.relay_id) return relayHref(node.relay_id);
  return null;
}

/** The node and its state in words, for its tooltip and screen readers. */
export function nodeTitle(node: MapNode): string {
  const state = node.state === "unknown" ? t("maps.state.unknown") : t(`common.state.${node.state}`);
  return t("maps.node.title", { name: node.name, state });
}

/** Counts of nodes in trouble, for the map's header. */
export function troubleCounts(live: MapLive): { critical: number; warning: number; down: number } {
  const shown = live.nodes.filter((n) => !n.hidden);
  return {
    critical: shown.filter((n) => n.state === "critical").length,
    warning: shown.filter((n) => n.state === "warning").length,
    down: shown.filter((n) => n.state === "offline" || n.state === "unreachable").length,
  };
}

/** The document with `key` placed at `x`, `y`, keeping whether it is hidden. */
export function place(document: MapDocument, key: string, x: number, y: number): MapDocument {
  const round = (v: number) => Math.round(v * 10) / 10;
  const existing = document.nodes.find((n) => n.key === key);
  const placement: MapPlacement = { key, x: round(x), y: round(y), ...(existing?.hidden && { hidden: true }) };
  return {
    ...document,
    nodes: existing ? document.nodes.map((n) => (n.key === key ? placement : n)) : [...document.nodes, placement],
  };
}

/** The document with `key` left off the map, or shown again. */
export function setHidden(document: MapDocument, key: string, hidden: boolean, at: { x: number; y: number }): MapDocument {
  const existing = document.nodes.find((n) => n.key === key);
  const placement: MapPlacement = { key, x: existing?.x ?? at.x, y: existing?.y ?? at.y, ...(hidden && { hidden: true }) };
  return {
    ...document,
    nodes: existing ? document.nodes.map((n) => (n.key === key ? placement : n)) : [...document.nodes, placement],
  };
}

/** Every node placed where `at` says, hidden ones kept hidden. */
export function placeAll(document: MapDocument, at: Map<string, { x: number; y: number }>): MapDocument {
  let next = document;
  for (const [key, point] of at) next = place(next, key, point.x, point.y);
  return next;
}

/** An id for a new zone, link or label, unused in `document`. */
export function newId(document: MapDocument, prefix: string): string {
  const taken = new Set([...document.zones, ...document.links, ...document.labels].map((item) => item.id));
  let n = 1;
  while (taken.has(`${prefix}${n}`)) n++;
  return `${prefix}${n}`;
}

/** The live map with the document's placements, zones, links and labels, to draw while editing. */
export function withDocument(live: MapLive, document: MapDocument): MapLive {
  const placements = new Map(document.nodes.map((n) => [n.key, n]));
  const keys = new Set(live.nodes.map((n) => n.key));
  const drawn = live.links.filter((l) => l.source !== "manual");
  const manual: MapLink[] = document.links
    .filter((l) => keys.has(l.from) && keys.has(l.to))
    .map((l) => {
      const before = live.links.find((old) => old.source === "manual" && old.id === l.id);
      return {
        id: l.id,
        from: l.from,
        to: l.to,
        source: "manual",
        state: before?.state ?? "unknown",
        label: l.label ?? null,
        from_port: null,
        to_port: null,
        interface: l.interface ?? null,
        traffic: before && before.interface?.instance === l.interface?.instance ? before.traffic : null,
      };
    });
  return {
    ...live,
    group_by_site: document.group_by_site,
    nodes: live.nodes.map((n) => {
      const placed = placements.get(n.key);
      return placed ? { ...n, x: placed.x, y: placed.y, hidden: !!placed.hidden } : { ...n, x: null, y: null, hidden: false };
    }),
    links: [...drawn, ...manual],
    zones: document.zones,
    labels: document.labels,
    background: live.background && { ...live.background, placement: document.background ?? null },
  };
}

/** An image file as base64, or why not. */
export async function imageBase64(file: File): Promise<{ data: string } | { error: string }> {
  if (!["image/png", "image/jpeg", "image/webp"].includes(file.type)) return { error: t("maps.background.type") };
  if (file.size > MAX_BACKGROUND_BYTES) return { error: t("maps.background.size") };
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return { data: btoa(binary) };
}

/** The size of an image at `src`, once loaded; null when it does not load. */
export function imageSize(src: string): Promise<{ width: number; height: number } | null> {
  return new Promise((resolve) => {
    const image = new Image();
    image.onload = () => resolve({ width: image.naturalWidth, height: image.naturalHeight });
    image.onerror = () => resolve(null);
    image.src = src;
  });
}

/** Where a signed-in browser reads a map's background, changing with the image. */
export function backgroundUrl(live: MapLive): string | null {
  return live.background ? `/api/maps/${encodeURIComponent(live.id)}/background?v=${encodeURIComponent(live.background.updated_at)}` : null;
}
