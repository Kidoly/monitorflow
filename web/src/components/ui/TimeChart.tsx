"use client";

import { type KeyboardEvent, type PointerEvent, type ReactNode, useId, useState } from "react";

import { formatNumber } from "@/lib/format";

import { niceTicks, nearestIndex, segments, timeTicks } from "./chartScale";
import { cx } from "./cx";
import { formatClock } from "./time";
import styles from "./TimeChart.module.css";

/** One line: `[unix seconds, value]` pairs sorted by time; null is a gap. */
export interface TimeSeries {
  label: string;
  points: [number, number | null][];
}

/** A dashed horizontal line with its label above it at the right: "Warning 80 %". */
export interface Threshold {
  severity: "warning" | "critical";
  value: number;
  /** "Warning"; the formatted value is appended. */
  label: string;
}

/**
 * A vertical line labelled at the top. `opened` and `resolved` are solid,
 * `now` dashed (an incident still open).
 */
export interface Marker {
  t: number;
  label: string;
  kind?: "opened" | "resolved" | "now";
}

/** A translucent band, such as the window an incident was open. */
export interface Band {
  start: number;
  /** Defaults to the end of the range: still open. */
  end?: number;
  severity: "warning" | "critical";
}

/** More series are not drawn; the palette has eight colours. */
export const MAX_SERIES = 8;

const W = 1000;
const H = 260;
const LEFT = 56;
const RIGHT = 980;
const TOP = 20;
const BOTTOM = 200;

/**
 * A line chart drawn as SVG in a `<figure>`: five y ticks on a nice scale,
 * x ticks every sixth of the range, dashed thresholds, markers and a band.
 * The plot is focusable: hover or ← → (Home, End, Escape) move a cursor
 * whose readout is also announced politely. Legend buttons
 * (`aria-pressed`) dim a series without removing it. Up to 8 series.
 */
export function TimeChart({
  title,
  subtitle,
  actions,
  series: allSeries,
  start: startProp,
  end: endProp,
  step,
  format = (v) => formatNumber(v),
  min,
  max,
  thresholds = [],
  markers = [],
  band,
  emptyText = "No data in this range.",
}: {
  title: string;
  /** Grey text beside the title: "Last 6 h · now 94.2 %". */
  subtitle?: ReactNode;
  /** Controls at the right of the title, such as the window buttons. */
  actions?: ReactNode;
  series: TimeSeries[];
  /** The x range in unix seconds; defaults to the data's. */
  start?: number;
  end?: number;
  /** Expected seconds between points; a longer gap breaks the line. */
  step?: number;
  /** Value to text, for ticks, thresholds and the readout. */
  format?: (value: number) => string;
  /** Fixed y bounds, like 0 and 100 for a percentage. */
  min?: number;
  max?: number;
  thresholds?: Threshold[];
  markers?: Marker[];
  band?: Band;
  emptyText?: string;
}) {
  const titleId = useId();
  const series = allSeries.slice(0, MAX_SERIES);
  const [hidden, setHidden] = useState<ReadonlySet<string>>(new Set());
  const [cursor, setCursor] = useState<number | null>(null);

  const times = [...new Set(series.flatMap((s) => s.points.map((p) => p[0])))].sort((a, b) => a - b);
  const values = series.flatMap((s) => s.points.map((p) => p[1])).filter((v): v is number => v !== null);
  const start = startProp ?? times[0] ?? 0;
  const end = endProp ?? times[times.length - 1] ?? start + 1;
  const yTicks = niceTicks(
    min ?? Math.min(0, ...values, ...thresholds.map((t) => t.value)),
    max ?? Math.max(...values, ...thresholds.map((t) => t.value), (min ?? 0) + 1),
  );
  const low = yTicks[0]!;
  const high = yTicks[yTicks.length - 1]!;
  const x = (t: number) => LEFT + ((t - start) / Math.max(end - start, 1)) * (RIGHT - LEFT);
  const y = (v: number) => BOTTOM - ((v - low) / (high - low)) * (BOTTOM - TOP);
  const daily = end - start > 86_400;
  const timeText = (t: number) => {
    const d = new Date(t * 1000);
    return daily ? `${d.toLocaleDateString("en-GB", { day: "2-digit", month: "short" })} ${formatClock(d).slice(0, 5)}` : formatClock(d);
  };

  const summary = [
    `${title}${series.length > 1 || series[0]?.label !== title ? `: ${series.map((s) => s.label).join(", ")}` : ""}`,
    ...thresholds.map((t) => `${t.label} ${format(t.value)}`),
    ...markers.map((m) => m.label),
  ].join(". ");

  const at = cursor === null ? null : times[cursor];
  const readout =
    at === undefined || at === null
      ? []
      : series
          .filter((s) => !hidden.has(s.label))
          .map((s) => ({ s, value: s.points.find((p) => p[0] === at)?.[1] ?? null }));
  const readoutText =
    at === undefined || at === null
      ? ""
      : `${timeText(at)}, ${readout.map((r) => `${r.s.label} ${r.value === null ? "no value" : format(r.value)}`).join(", ")}`;

  function onPointerMove(event: PointerEvent<SVGRectElement>) {
    const svg = event.currentTarget.ownerSVGElement;
    const rect = svg?.getBoundingClientRect();
    if (!rect || rect.width === 0 || times.length === 0) return;
    const svgX = ((event.clientX - rect.left) / rect.width) * W;
    setCursor(nearestIndex(times, start + ((svgX - LEFT) / (RIGHT - LEFT)) * (end - start)));
  }

  function onKeyDown(event: KeyboardEvent<SVGRectElement>) {
    if (times.length === 0) return;
    const last = times.length - 1;
    const current = cursor ?? last;
    const next =
      event.key === "ArrowLeft" ? Math.max(0, cursor === null ? last : current - 1)
      : event.key === "ArrowRight" ? Math.min(last, cursor === null ? last : current + 1)
      : event.key === "Home" ? 0
      : event.key === "End" ? last
      : undefined;
    if (event.key === "Escape") {
      setCursor(null);
      return;
    }
    if (next === undefined) return;
    event.preventDefault();
    setCursor(next);
  }

  function toggle(label: string) {
    setHidden((current) => {
      const next = new Set(current);
      if (!next.delete(label)) next.add(label);
      return next;
    });
  }

  const header = (
    <div className={styles.head}>
      <h2 id={titleId} className={styles.title}>
        {title}
      </h2>
      {subtitle && <span className={styles.subtitle}>{subtitle}</span>}
      {actions && <div className={styles.actions}>{actions}</div>}
    </div>
  );

  if (values.length === 0) {
    return (
      <figure className={styles.figure} aria-labelledby={titleId}>
        <figcaption>{header}</figcaption>
        <p className={styles.empty}>{emptyText}</p>
      </figure>
    );
  }

  const boxWidth = 40 + 6.5 * Math.max(timeText(at ?? start).length, ...readout.map((r) => r.s.label.length + 2 + (r.value === null ? 1 : format(r.value).length)));
  const boxHeight = 24 + 16 * readout.length;
  const boxX = at === null || at === undefined ? 0 : x(at) + 8 + boxWidth > RIGHT ? x(at) - 8 - boxWidth : x(at) + 8;

  return (
    <figure className={styles.figure} aria-labelledby={titleId}>
      <figcaption>
        {header}
        <span className="visually-hidden">{summary}.</span>
      </figcaption>
      {/* A group, not an img: an img's children are hidden, the focusable cursor included. */}
      <svg className={styles.svg} viewBox={`0 0 ${W} ${H}`} role="group" aria-label={summary}>
        <g aria-hidden="true">
          <g className={styles.grid} strokeWidth={1}>
            {yTicks.map((v) => (
              <line key={v} x1={LEFT} y1={y(v)} x2={RIGHT} y2={y(v)} />
            ))}
          </g>
          <g textAnchor="end">
            {yTicks.map((v) => (
              <text key={v} x={LEFT - 8} y={y(v) + 4}>
                {v === 0 ? "0" : format(v)}
              </text>
            ))}
          </g>
          <g textAnchor="middle">
            {timeTicks(start, end).map((tick) => (
              <text key={tick.t} x={x(tick.t)} y={BOTTOM + 24}>
                {tick.label}
              </text>
            ))}
          </g>
          {band && (
            <rect
              data-testid="chart-band"
              x={x(Math.max(band.start, start))}
              y={TOP}
              width={Math.max(0, x(Math.min(band.end ?? end, end)) - x(Math.max(band.start, start)))}
              height={BOTTOM - TOP}
              fill={band.severity === "critical" ? "var(--threshold-crit)" : "var(--threshold-warn)"}
              opacity={0.06}
            />
          )}
          {thresholds.map((t) => (
            <g key={`${t.severity}-${t.value}`} className={styles[t.severity]} data-testid="chart-threshold">
              <line x1={LEFT} y1={y(t.value)} x2={RIGHT} y2={y(t.value)} strokeDasharray="4 3" strokeWidth={1.5} />
              <text x={RIGHT - 4} y={y(t.value) - 4} textAnchor="end">
                {t.label} {format(t.value)}
              </text>
            </g>
          ))}
          {series.map((s, i) => (
            <g key={s.label} className={cx(hidden.has(s.label) && styles.off)} data-testid="chart-series">
              {segments(s.points, step).map((segment) => (
                <polyline
                  key={segment[0]![0]}
                  className={styles.line}
                  stroke={`var(--series-${i + 1})`}
                  points={segment.map(([t, v]) => `${x(t)},${y(v)}`).join(" ")}
                />
              ))}
            </g>
          ))}
          {markers
            .filter((m) => m.t >= start && m.t <= end)
            .map((m) => {
              const nearEnd = x(m.t) > RIGHT - 140;
              return (
                <g key={`${m.label}-${m.t}`} className={styles.marker} data-testid="chart-marker">
                  <line
                    x1={x(m.t)}
                    y1={TOP - 8}
                    x2={x(m.t)}
                    y2={BOTTOM}
                    strokeWidth={1.5}
                    strokeDasharray={m.kind === "now" ? "3 3" : undefined}
                  />
                  <text x={nearEnd ? x(m.t) - 4 : x(m.t) + 4} y={TOP - 6} textAnchor={nearEnd ? "end" : "start"}>
                    {m.label}
                  </text>
                </g>
              );
            })}
          {at !== null && at !== undefined && (
            <>
              <line className={styles.guide} x1={x(at)} y1={TOP} x2={x(at)} y2={BOTTOM} strokeDasharray="2 2" opacity={0.5} />
              <g className={styles.readout}>
                <rect className={styles.box} x={boxX} y={TOP + 40} width={boxWidth} height={boxHeight} rx={6} />
                <text className={styles.time} x={boxX + 8} y={TOP + 56}>
                  {timeText(at)}
                </text>
                {readout.map((r, i) => (
                  <g key={r.s.label}>
                    <rect
                      x={boxX + 8}
                      y={TOP + 65 + 16 * i}
                      width={10}
                      height={3}
                      fill={`var(--series-${series.indexOf(r.s) + 1})`}
                    />
                    <text x={boxX + 24} y={TOP + 71 + 16 * i}>
                      {r.s.label}
                      {"  "}
                      {r.value === null ? "—" : format(r.value)}
                    </text>
                  </g>
                ))}
              </g>
            </>
          )}
        </g>
        <rect
          className={styles.plot}
          x={LEFT}
          y={TOP}
          width={RIGHT - LEFT}
          height={BOTTOM - TOP}
          tabIndex={0}
          role="slider"
          aria-label={`${title}: the left and right arrow keys move the cursor`}
          aria-valuemin={0}
          aria-valuemax={Math.max(0, times.length - 1)}
          aria-valuenow={cursor ?? times.length - 1}
          aria-valuetext={at === null || at === undefined ? "No cursor" : timeText(at)}
          onPointerMove={onPointerMove}
          onPointerLeave={() => setCursor(null)}
          onKeyDown={onKeyDown}
        />
      </svg>
      <p className="visually-hidden" aria-live="polite">
        {readoutText}
      </p>
      <div className={styles.legend}>
        {series.map((s, i) => (
          <button
            key={s.label}
            type="button"
            className={styles.toggle}
            aria-pressed={!hidden.has(s.label)}
            onClick={() => toggle(s.label)}
          >
            <i className={styles.swatch} style={{ background: `var(--series-${i + 1})` }} aria-hidden="true" />
            {s.label}
          </button>
        ))}
        {thresholds.map((t) => (
          <span key={`${t.severity}-${t.value}`} className={styles.key}>
            <i
              className={styles.swatch}
              style={{ background: t.severity === "critical" ? "var(--threshold-crit)" : "var(--threshold-warn)" }}
              aria-hidden="true"
            />
            {t.label} {format(t.value)}
          </span>
        ))}
        {allSeries.length > MAX_SERIES && (
          <span className={styles.subtitle}>{allSeries.length - MAX_SERIES} more series not shown.</span>
        )}
      </div>
    </figure>
  );
}
