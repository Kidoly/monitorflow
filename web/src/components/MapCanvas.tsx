"use client";

import { useRouter } from "next/navigation";
import { type KeyboardEvent, type PointerEvent, type WheelEvent, useEffect, useMemo, useRef, useState } from "react";

import { t } from "@/i18n";
import type { MapLink, MapLive, MapNode } from "@/lib/api";
import { type Box, type Point, bounds, positions, union } from "@/lib/mapLayout";
import { backgroundUrl, imageSize, linkColor, linkWidth, nodeColor, nodeHref, nodeTitle, trafficText } from "@/lib/maps";

import styles from "./Maps.module.css";

/** What is selected while editing. */
export type MapSelection =
  | { kind: "node"; key: string }
  | { kind: "zone"; id: string }
  | { kind: "label"; id: string }
  | { kind: "link"; id: string };

/** Radius of a node, in the map's units. */
export const NODE_RADIUS = 22;
const LABEL_SIZES = { small: 14, medium: 20, large: 30 } as const;
/** Zoom limits, as a share of the map's own size. */
const MIN_SPAN = 120;
const MAX_SPAN = 400_000;

type Drag =
  | { kind: "pan"; start: Point; view: Box; moved: boolean }
  | { kind: "node"; key: string; offset: Point; at: Point; moved: boolean }
  | { kind: "zone"; id: string; offset: Point; box: Box; resize: boolean; moved: boolean }
  | { kind: "label"; id: string; offset: Point; at: Point; moved: boolean };

export interface MapCanvasProps {
  live: MapLive;
  /** The background image's address: a URL or a data URL. */
  background?: string | null;
  /** Its own size, for a background without a placement. */
  backgroundSize?: { width: number; height: number } | null;
  /** `screen`: no links, no controls, always fitted (ADR-0099). */
  mode: "page" | "panel" | "screen";
  editing?: boolean;
  /** Clicks pick nodes, for drawing a link. */
  picking?: boolean;
  selected?: MapSelection | null;
  onSelect?: (selection: MapSelection | null) => void;
  onPick?: (node: MapNode) => void;
  onMoveNode?: (key: string, at: Point) => void;
  onMoveZone?: (id: string, box: Box) => void;
  onMoveLabel?: (id: string, at: Point) => void;
  /** Changing it fits the map to the view again. */
  fitKey?: number;
  label: string;
}

/** The world box around everything drawn. */
function contentBox(live: MapLive, at: Map<string, Point>, background: Box | null): Box {
  const nodes = bounds([...at.values()], NODE_RADIUS + 60);
  const zones = live.zones.map((z) => ({ x: z.x, y: z.y, width: z.width, height: z.height }));
  const labels = live.labels.map((l) => ({ x: l.x - 10, y: l.y - 40, width: l.text.length * 16 + 20, height: 60 }));
  return union([nodes, ...zones, ...labels, background]) ?? { x: -400, y: -300, width: 800, height: 600 };
}

/** The view that shows `box`, a little larger. */
function fitted(box: Box): Box {
  const pad = Math.max(box.width, box.height) * 0.04;
  return { x: box.x - pad, y: box.y - pad, width: Math.max(box.width + 2 * pad, MIN_SPAN), height: Math.max(box.height + 2 * pad, MIN_SPAN) };
}

/** Draws a network map in SVG: zoomed with the wheel or the buttons, moved by dragging, never scrolled. */
export function MapCanvas({
  live,
  background,
  backgroundSize,
  mode,
  editing = false,
  picking = false,
  selected = null,
  onSelect,
  onPick,
  onMoveNode,
  onMoveZone,
  onMoveLabel,
  fitKey = 0,
  label,
}: MapCanvasProps) {
  const router = useRouter();
  const svg = useRef<SVGSVGElement | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const at = useMemo(() => positions(live.nodes, live.links), [live.nodes, live.links]);
  const backgroundBox: Box | null = useMemo(() => {
    if (!live.background || !background) return null;
    const placement = live.background.placement;
    if (placement) return placement;
    return backgroundSize ? { x: 0, y: 0, width: backgroundSize.width, height: backgroundSize.height } : null;
  }, [live.background, background, backgroundSize]);
  const content = useMemo(() => contentBox(live, at, backgroundBox), [live, at, backgroundBox]);
  const [view, setView] = useState<Box>(() => fitted(content));

  // Fit when asked, when the content first shows, and always on a screen.
  const fitOn = mode === "screen" ? JSON.stringify(content) : `${fitKey}|${live.id}|${backgroundBox ? 1 : 0}`;
  const [fittedFor, setFittedFor] = useState(fitOn);
  if (fittedFor !== fitOn) {
    setFittedFor(fitOn);
    setView(fitted(content));
  }

  const shown = new Map(live.nodes.filter((n) => !n.hidden).map((n) => [n.key, n]));
  const where = (key: string): Point | undefined => {
    if (drag?.kind === "node" && drag.key === key) return drag.at;
    return at.get(key);
  };

  /** The world point under a pointer. */
  function world(clientX: number, clientY: number): Point {
    const element = svg.current;
    const matrix = element?.getScreenCTM?.();
    if (!element || !matrix) return { x: clientX, y: clientY };
    const point = element.createSVGPoint();
    point.x = clientX;
    point.y = clientY;
    const p = point.matrixTransform(matrix.inverse());
    return { x: p.x, y: p.y };
  }

  function zoom(factor: number, around?: Point) {
    setView((v) => {
      const width = Math.min(MAX_SPAN, Math.max(MIN_SPAN, v.width * factor));
      const k = width / v.width;
      const centre = around ?? { x: v.x + v.width / 2, y: v.y + v.height / 2 };
      return { x: centre.x - (centre.x - v.x) * k, y: centre.y - (centre.y - v.y) * k, width, height: v.height * k };
    });
  }

  function onWheel(event: WheelEvent<SVGSVGElement>) {
    if (mode === "screen") return;
    event.preventDefault();
    zoom(event.deltaY > 0 ? 1.15 : 1 / 1.15, world(event.clientX, event.clientY));
  }

  // React's wheel listener is passive: a native one stops the page from scrolling under the map.
  useEffect(() => {
    const element = svg.current;
    if (!element || mode === "screen") return;
    const stop = (event: globalThis.WheelEvent) => event.preventDefault();
    element.addEventListener("wheel", stop, { passive: false });
    return () => element.removeEventListener("wheel", stop);
  }, [mode]);

  function capture(event: PointerEvent<SVGElement>) {
    try {
      svg.current?.setPointerCapture(event.pointerId);
    } catch {
      // Old browsers without capture still drag while the pointer stays over the map.
    }
  }

  function startPan(event: PointerEvent<SVGSVGElement>) {
    if (mode === "screen" || event.button !== 0) return;
    capture(event);
    setDrag({ kind: "pan", start: { x: event.clientX, y: event.clientY }, view, moved: false });
  }

  function startNode(event: PointerEvent<SVGGElement>, node: MapNode) {
    if (mode === "screen" || event.button !== 0) return;
    event.stopPropagation();
    capture(event);
    const point = world(event.clientX, event.clientY);
    const p = at.get(node.key) ?? point;
    setDrag({ kind: "node", key: node.key, offset: { x: point.x - p.x, y: point.y - p.y }, at: p, moved: false });
  }

  function startZone(event: PointerEvent<SVGElement>, id: string, box: Box, resize: boolean) {
    if (!editing || event.button !== 0) return;
    event.stopPropagation();
    capture(event);
    const point = world(event.clientX, event.clientY);
    const origin = resize ? { x: box.x + box.width, y: box.y + box.height } : { x: box.x, y: box.y };
    setDrag({ kind: "zone", id, offset: { x: point.x - origin.x, y: point.y - origin.y }, box, resize, moved: false });
  }

  function startLabel(event: PointerEvent<SVGElement>, id: string, p: Point) {
    if (!editing || event.button !== 0) return;
    event.stopPropagation();
    capture(event);
    const point = world(event.clientX, event.clientY);
    setDrag({ kind: "label", id, offset: { x: point.x - p.x, y: point.y - p.y }, at: p, moved: false });
  }

  function onMove(event: PointerEvent<SVGSVGElement>) {
    if (!drag) return;
    if (drag.kind === "pan") {
      const matrix = svg.current?.getScreenCTM?.();
      const scale = matrix?.a || 1;
      const dx = (event.clientX - drag.start.x) / scale;
      const dy = (event.clientY - drag.start.y) / scale;
      setView({ ...drag.view, x: drag.view.x - dx, y: drag.view.y - dy });
      if (Math.abs(dx) + Math.abs(dy) > 2) setDrag({ ...drag, moved: true });
      return;
    }
    if (!editing) return;
    const point = world(event.clientX, event.clientY);
    const next = { x: point.x - drag.offset.x, y: point.y - drag.offset.y };
    if (drag.kind === "node" || drag.kind === "label") setDrag({ ...drag, at: next, moved: true });
    if (drag.kind === "zone") {
      const box = drag.resize
        ? { ...drag.box, width: Math.max(40, next.x - drag.box.x), height: Math.max(40, next.y - drag.box.y) }
        : { ...drag.box, x: next.x, y: next.y };
      setDrag({ ...drag, box, moved: true });
    }
  }

  function onUp() {
    const done = drag;
    setDrag(null);
    if (!done) return;
    if (done.kind === "pan") {
      if (!done.moved && editing) onSelect?.(null);
      return;
    }
    if (done.kind === "node") {
      const node = shown.get(done.key);
      if (done.moved && editing) onMoveNode?.(done.key, done.at);
      else if (node) click(node);
    }
    if (done.kind === "zone") {
      if (done.moved) onMoveZone?.(done.id, done.box);
      onSelect?.({ kind: "zone", id: done.id });
    }
    if (done.kind === "label") {
      if (done.moved) onMoveLabel?.(done.id, done.at);
      onSelect?.({ kind: "label", id: done.id });
    }
  }

  function click(node: MapNode) {
    if (picking) return onPick?.(node);
    if (editing) return onSelect?.({ kind: "node", key: node.key });
    const href = mode === "screen" ? null : nodeHref(node);
    if (href) router.push(href);
  }

  function onNodeKey(event: KeyboardEvent<SVGGElement>, node: MapNode) {
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      click(node);
    }
  }

  // Zones around each site's nodes, for a map of several sites.
  const siteZones =
    live.group_by_site && live.sites.length > 1
      ? live.sites
          .map((site) => {
            const points = live.nodes
              .filter((n) => !n.hidden && n.site_id === site.id)
              .map((n) => where(n.key))
              .filter((p): p is Point => !!p);
            const box = bounds(points, NODE_RADIUS + 46);
            return box && { site, box };
          })
          .filter((z): z is { site: { id: string; name: string }; box: Box } => !!z)
      : [];

  const linkLine = (link: MapLink) => {
    const a = where(link.from);
    const b = where(link.to);
    if (!a || !b || !shown.has(link.from) || !shown.has(link.to)) return null;
    const isSelected = selected?.kind === "link" && selected.id === link.id;
    const text = [link.label, trafficText(link)].filter(Boolean).join("  ");
    const middle = { x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 };
    const title = [shown.get(link.from)?.name, shown.get(link.to)?.name].join(" – ");
    return (
      <g
        key={`link-${link.source}-${link.id}`}
        className={styles.link}
        onPointerDown={(e) => {
          if (editing && link.source === "manual") {
            e.stopPropagation();
            onSelect?.({ kind: "link", id: link.id });
          }
        }}
      >
        <title>{title}</title>
        {isSelected && <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} className={styles.linkHalo} strokeWidth={linkWidth(link) + 8} />}
        <line
          x1={a.x}
          y1={a.y}
          x2={b.x}
          y2={b.y}
          stroke={linkColor(link.state)}
          strokeWidth={linkWidth(link)}
          strokeDasharray={link.state === "unknown" ? "6 6" : undefined}
          data-state={link.state}
        />
        {editing && link.source === "manual" && <line x1={a.x} y1={a.y} x2={b.x} y2={b.y} className={styles.linkHit} />}
        {text && (
          <text x={middle.x} y={middle.y - 8} className={styles.linkText} textAnchor="middle">
            {text}
          </text>
        )}
      </g>
    );
  };

  const backgroundOpacity = live.background?.placement?.opacity ?? 1;
  const viewBox = `${view.x} ${view.y} ${view.width} ${view.height}`;
  return (
    <div className={`${styles.canvas} ${mode === "screen" ? styles.canvasScreen : ""}`}>
      <svg
        ref={svg}
        className={`${styles.svg} ${drag?.kind === "pan" ? styles.panning : ""} ${picking ? styles.picking : ""}`}
        viewBox={viewBox}
        preserveAspectRatio="xMidYMid meet"
        role="img"
        aria-label={label}
        onPointerDown={startPan}
        onPointerMove={onMove}
        onPointerUp={onUp}
        onPointerCancel={() => setDrag(null)}
        onWheel={onWheel}
      >
        {background && backgroundBox && (
          <image
            href={background}
            x={backgroundBox.x}
            y={backgroundBox.y}
            width={backgroundBox.width}
            height={backgroundBox.height}
            opacity={backgroundOpacity}
            preserveAspectRatio="none"
          />
        )}
        {siteZones.map(({ site, box }) => (
          <g key={`site-${site.id}`} className={styles.siteZone}>
            <rect x={box.x} y={box.y} width={box.width} height={box.height} rx={18} />
            <text x={box.x + 16} y={box.y + 26}>
              {site.name}
            </text>
          </g>
        ))}
        {live.zones.map((zone) => {
          const box = drag?.kind === "zone" && drag.id === zone.id ? drag.box : zone;
          const isSelected = selected?.kind === "zone" && selected.id === zone.id;
          const color = `var(--series-${zone.color})`;
          return (
            <g key={`zone-${zone.id}`} className={`${styles.zone} ${editing ? styles.movable : ""}`}>
              <rect
                x={box.x}
                y={box.y}
                width={box.width}
                height={box.height}
                rx={12}
                fill={color}
                stroke={color}
                className={isSelected ? styles.selectedZone : undefined}
                onPointerDown={(e) => startZone(e, zone.id, box, false)}
              />
              {zone.label && (
                <text x={box.x + 14} y={box.y + 28} className={styles.zoneLabel}>
                  {zone.label}
                </text>
              )}
              {editing && isSelected && (
                <rect
                  className={styles.handle}
                  x={box.x + box.width - 10}
                  y={box.y + box.height - 10}
                  width={20}
                  height={20}
                  onPointerDown={(e) => startZone(e, zone.id, box, true)}
                >
                  <title>{t("maps.edit.resize")}</title>
                </rect>
              )}
            </g>
          );
        })}
        {live.links.map(linkLine)}
        {[...shown.values()].map((node) => {
          const p = where(node.key);
          if (!p) return null;
          const isSelected = selected?.kind === "node" && selected.key === node.key;
          const href = mode !== "screen" && !editing && !picking ? nodeHref(node) : null;
          return (
            <g
              key={node.key}
              className={`${styles.node} ${href || editing || picking ? styles.clickable : ""}`}
              transform={`translate(${p.x} ${p.y})`}
              onPointerDown={(e) => startNode(e, node)}
              onKeyDown={(e) => onNodeKey(e, node)}
              tabIndex={mode === "screen" ? undefined : 0}
              role={href ? "link" : mode === "screen" ? undefined : "button"}
              aria-label={nodeTitle(node)}
              data-state={node.state}
              data-key={node.key}
            >
              <title>{nodeTitle(node)}</title>
              {isSelected && <circle r={NODE_RADIUS + 7} className={styles.selectedNode} />}
              <NodeShape node={node} />
              <text y={NODE_RADIUS + 18} textAnchor="middle" className={styles.nodeName}>
                {node.name.length > 28 ? `${node.name.slice(0, 27)}…` : node.name}
              </text>
            </g>
          );
        })}
        {live.labels.map((text) => {
          const p = drag?.kind === "label" && drag.id === text.id ? drag.at : text;
          const isSelected = selected?.kind === "label" && selected.id === text.id;
          return (
            <text
              key={`label-${text.id}`}
              x={p.x}
              y={p.y}
              fontSize={LABEL_SIZES[text.size]}
              className={`${styles.mapLabel} ${editing ? styles.movable : ""} ${isSelected ? styles.selectedLabel : ""}`}
              onPointerDown={(e) => startLabel(e, text.id, p)}
            >
              {text.text}
            </text>
          );
        })}
      </svg>
      {mode !== "screen" && (
        <div className={styles.zoom}>
          <button type="button" onClick={() => zoom(1 / 1.3)} aria-label={t("maps.zoom.in")} title={t("maps.zoom.in")}>
            +
          </button>
          <button type="button" onClick={() => zoom(1.3)} aria-label={t("maps.zoom.out")} title={t("maps.zoom.out")}>
            −
          </button>
          <button type="button" onClick={() => setView(fitted(content))} title={t("maps.zoom.fit")}>
            {t("maps.zoom.fitShort")}
          </button>
        </div>
      )}
    </div>
  );
}

/** A node's shape by its kind, filled with its state's colour. */
function NodeShape({ node }: { node: MapNode }) {
  const fill = nodeColor(node.state);
  const r = NODE_RADIUS;
  switch (node.kind) {
    case "central": {
      const points = [0, 1, 2, 3, 4, 5].map((i) => {
        const a = (Math.PI / 3) * i - Math.PI / 2;
        return `${(Math.cos(a) * (r + 4)).toFixed(1)},${(Math.sin(a) * (r + 4)).toFixed(1)}`;
      });
      return <polygon points={points.join(" ")} fill={fill} className={styles.shape} />;
    }
    case "relay":
      return <polygon points={`0,${-r - 3} ${r + 3},0 0,${r + 3} ${-r - 3},0`} fill={fill} className={styles.shape} />;
    case "device":
      return (
        <g>
          <rect x={-r} y={-r} width={2 * r} height={2 * r} rx={7} fill={fill} className={styles.shape} />
          <DeviceMark template={node.template} />
        </g>
      );
    case "host":
    case "neighbor":
      return <circle r={r - 2} fill="var(--surface)" stroke={fill} strokeWidth={3} strokeDasharray="5 4" />;
    default:
      return (
        <g>
          <circle r={r} fill={fill} className={styles.shape} />
          <rect x={-9} y={-9} width={18} height={6} rx={1.5} className={styles.mark} />
          <rect x={-9} y={1} width={18} height={6} rx={1.5} className={styles.mark} />
        </g>
      );
  }
}

/** A small mark of what a device is: arrows for a switch, a cross for a router, bricks for a firewall. */
function DeviceMark({ template }: { template: string | null }) {
  switch (template) {
    case "switch":
      return <path d="M-11 -5 H9 M5 -9 L9 -5 L5 -1 M11 5 H-9 M-5 1 L-9 5 L-5 9" className={styles.markLine} />;
    case "router":
      return <path d="M0 -11 V11 M-11 0 H11 M-4 -7 L0 -11 L4 -7 M-4 7 L0 11 L4 7" className={styles.markLine} />;
    case "firewall":
      return <path d="M-11 -7 H11 M-11 0 H11 M-11 7 H11 M-4 -7 V0 M5 0 V7 M-2 7 V11" className={styles.markLine} />;
    default:
      return <rect x={-8} y={-8} width={16} height={16} rx={3} className={styles.markOutline} />;
  }
}

/** Loads a map's background where it can be read, and its size; null without one. */
export function useBackground(
  live: MapLive | null,
  load?: (live: MapLive) => Promise<string | null>,
): { src: string; size: { width: number; height: number } | null } | null {
  const [found, setFound] = useState<{ src: string; size: { width: number; height: number } | null; version: string } | null>(null);
  const version = live?.background?.updated_at ?? null;
  const id = live?.id ?? null;
  useEffect(() => {
    if (!live || !version) return;
    let gone = false;
    void (async () => {
      try {
        const src = load ? await load(live) : backgroundUrl(live);
        if (!src || gone) return;
        const size = await imageSize(src);
        if (!gone) setFound({ src, size, version: `${id}|${version}` });
      } catch {
        // No background is better than no map.
      }
    })();
    return () => {
      gone = true;
    };
  }, [id, version]); // eslint-disable-line react-hooks/exhaustive-deps
  return found && version && found.version === `${id}|${version}` ? found : null;
}

/** A map as a dashboard panel or on a screen: read-only, with its background. */
export function LiveMap({
  live,
  mode,
  loadBackground,
}: {
  live: MapLive;
  mode: "panel" | "screen";
  loadBackground?: (live: MapLive) => Promise<string | null>;
}) {
  const background = useBackground(live, loadBackground);
  return (
    <MapCanvas
      live={live}
      mode={mode}
      background={background?.src}
      backgroundSize={background?.size}
      label={t("maps.canvas.label", { name: live.name })}
    />
  );
}
