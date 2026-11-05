"use client";

import { type ReactNode, useLayoutEffect, useRef, useState } from "react";

import type { DashboardData, Panel, PanelData } from "@/lib/api";
import { unitOf } from "@/lib/format";
import { incidentTitle } from "@/lib/incidents";
import { formatSensorValue } from "@/lib/sensors";
import { niceTicks, segments, timeTicks } from "@/components/ui/chartScale";
import { formatSince } from "@/components/ui/time";

import { emptyReason } from "./DashboardPanel";
import styles from "./Display.module.css";

/** Chart text in SVG units; the drawing is scaled so it matches the panel's text. */
const TEXT = 16;

/**
 * Lines over time for a screen: the SVG is sized to the space it gets,
 * measured on load and when the window changes size (no ResizeObserver,
 * which old TVs lack), so its text stays as large as the panel's. Each
 * line's last value is in the legend, as nobody points at a wall.
 */
function ScreenChart({
  series,
  start,
  end,
  step,
  format,
  ratio,
}: {
  series: { label: string; points: [number, number | null][] }[];
  start: number;
  end: number;
  step: number;
  format: (v: number) => string;
  ratio: boolean;
}) {
  const box = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 600, h: 240 });

  useLayoutEffect(() => {
    const measure = () => {
      const el = box.current;
      if (!el || el.clientWidth === 0 || el.clientHeight === 0) return;
      const font = parseFloat(window.getComputedStyle(el).fontSize) || TEXT;
      const scale = font / TEXT;
      setSize({ w: el.clientWidth / scale, h: el.clientHeight / scale });
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);

  const values = series.flatMap((s) => s.points.map((p) => p[1])).filter((v): v is number => v !== null);
  const ticks = niceTicks(ratio ? 0 : Math.min(0, ...values), ratio ? 1 : Math.max(...values, 1), 3);
  const low = ticks[0]!;
  const high = ticks[ticks.length - 1]!;
  const left = TEXT * 4.5;
  const right = size.w - TEXT;
  const top = TEXT * 0.8;
  const bottom = size.h - TEXT * 1.8;
  const x = (t: number) => left + ((t - start) / Math.max(end - start, 1)) * (right - left);
  const y = (v: number) => bottom - ((v - low) / (high - low || 1)) * (bottom - top);
  const last = (points: [number, number | null][]) => [...points].reverse().find((p) => p[1] !== null)?.[1] ?? null;

  return (
    <>
      <div ref={box} className={styles.plot}>
        <svg viewBox={`0 0 ${size.w} ${size.h}`} preserveAspectRatio="none" aria-hidden="true">
          {ticks.map((v) => (
            <g key={v}>
              <line className={styles.gridLine} x1={left} x2={right} y1={y(v)} y2={y(v)} />
              <text className={styles.axis} x={left - TEXT / 2} y={y(v) + TEXT / 3} textAnchor="end" fontSize={TEXT}>
                {v === 0 ? "0" : format(v)}
              </text>
            </g>
          ))}
          {timeTicks(start, end)
            .filter((_, i) => i % 2 === 0)
            .map((tick) => (
              <text key={tick.t} className={styles.axis} x={x(tick.t)} y={size.h - TEXT * 0.3} textAnchor="middle" fontSize={TEXT}>
                {tick.label}
              </text>
            ))}
          {series.map((s, i) =>
            segments(s.points, step).map((segment) => (
              <polyline
                key={`${s.label}-${segment[0]![0]}`}
                className={styles.line}
                stroke={`var(--series-${i + 1})`}
                points={segment.map(([t, v]) => `${x(t)},${y(v)}`).join(" ")}
              />
            )),
          )}
        </svg>
      </div>
      <ul className={styles.legend}>
        {series.map((s, i) => {
          const value = last(s.points);
          return (
            <li key={s.label}>
              <i className={styles.swatch} style={{ background: `var(--series-${i + 1})` }} aria-hidden="true" />
              {s.label} <b>{value === null ? "–" : format(value)}</b>
            </li>
          );
        })}
      </ul>
    </>
  );
}

function Frame({ panel, children }: { panel: Panel; children: ReactNode }) {
  return (
    <section className={`${styles.panel} ${styles[`w${panel.width}`]}`} aria-label={panel.title}>
      <h2 className={styles.title}>{panel.title}</h2>
      {children}
    </section>
  );
}

/**
 * One panel on a screen: as on the dashboard page, larger, and with no
 * links, since a screen leads nowhere.
 */
export function ScreenPanel({
  panel,
  data,
  window,
}: {
  panel: Panel;
  data: PanelData | undefined;
  window: Pick<DashboardData, "start" | "end" | "step" | "at"> | undefined;
}) {
  const metric = panel.metric ?? "";
  const format = (v: number) => formatSensorValue(metric, v);
  if (!data || !window) {
    return (
      <Frame panel={panel}>
        <p className={styles.muted}>Loading…</p>
      </Frame>
    );
  }
  if (data.kind === "error") {
    return (
      <Frame panel={panel}>
        <p className={styles.bad}>{data.error}</p>
      </Frame>
    );
  }
  const empty = emptyReason(panel, data);
  if (empty) {
    return (
      <Frame panel={panel}>
        <p className={styles.muted}>{empty}</p>
      </Frame>
    );
  }
  switch (data.kind) {
    case "chart":
      return (
        <Frame panel={panel}>
          {data.series.some((s) => s.points.some((p) => p[1] !== null)) ? (
            <ScreenChart
              series={data.series}
              start={window.start}
              end={window.end}
              step={window.step}
              format={format}
              ratio={unitOf(metric) === "ratio"}
            />
          ) : (
            <p className={styles.muted}>No data in this range.</p>
          )}
        </Frame>
      );
    case "figure":
      return (
        <Frame panel={panel}>
          <p className={styles.figure}>
            {data.value === null ? "–" : panel.combine === "count" ? data.value.toLocaleString("en") : format(data.value)}
          </p>
          <p className={styles.muted}>
            {data.value === null ? "No recent value" : `${data.servers} ${data.servers === 1 ? "server" : "servers"}`}
          </p>
        </Frame>
      );
    case "top":
      return (
        <Frame panel={panel}>
          {data.rows.length === 0 ? (
            <p className={styles.muted}>No recent values.</p>
          ) : (
            <ol className={styles.rows}>
              {data.rows.map((row) => (
                <li key={`${row.server_id}/${row.instance ?? ""}`}>
                  <span className={styles.name}>
                    {row.hostname}
                    {row.instance && <span className={styles.muted}> {row.instance}</span>}
                  </span>
                  <b>{format(row.value ?? Number.NaN)}</b>
                </li>
              ))}
            </ol>
          )}
        </Frame>
      );
    case "alerts": {
      const now = new Date(window.at * 1000);
      return (
        <Frame panel={panel}>
          {data.alerts.length === 0 ? (
            <p className={styles.ok}>No open alerts</p>
          ) : (
            <ul className={styles.rows}>
              {data.alerts.map((alert) => (
                <li key={alert.id} className={alert.severity === "critical" ? styles.critical : styles.warning}>
                  <span className={styles.name}>
                    <b>{alert.severity === "critical" ? "Critical" : "Warning"}</b> {incidentTitle(alert)}{" "}
                    <span className={styles.muted}>on {alert.hostname}</span>
                  </span>
                  <span className={styles.muted}>{formatSince(new Date(alert.opened_at), now)}</span>
                </li>
              ))}
            </ul>
          )}
        </Frame>
      );
    }
  }
}
