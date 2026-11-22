"use client";

import { type ReactNode, useLayoutEffect, useRef, useState } from "react";

import { t } from "@/i18n";
import type { DashboardData, Panel, PanelData } from "@/lib/api";
import { endWords } from "@/lib/dependencies";
import { heatColor, levelColor, panelAdditive, panelColor, panelFormat, panelScale, thresholdBands, worstFirst } from "@/lib/dashboards";
import { formatSensorValue } from "@/lib/sensors";
import { incidentTitle } from "@/lib/incidents";
import { BarList } from "@/components/ui/BarList";
import { ChartMarks, type ChartVariant, shapeSeries } from "@/components/ui/ChartMarks";
import { niceTicks, timeTicks } from "@/components/ui/chartScale";
import { Gauge } from "@/components/ui/Gauge";
import { Heatmap } from "@/components/ui/Heatmap";
import { Markdown } from "@/components/ui/Markdown";
import { SkeletonLines } from "@/components/ui/PageState";
import { formatSince } from "@/components/ui/time";

import { chartBounds, chartThresholds, columnTitle, emptyReason, quiet, seriesLabel, variantOf } from "./DashboardPanel";
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
  min,
  max,
  variant,
  additive,
  color,
  legend,
  thresholds,
}: {
  series: { label: string; points: [number, number | null][] }[];
  start: number;
  end: number;
  step: number;
  format: (v: number) => string;
  min?: number;
  max?: number;
  variant: ChartVariant;
  /** False draws several areas or columns over each other. */
  additive: boolean;
  color: (index: number) => string;
  legend: boolean;
  thresholds: { severity: "warning" | "critical"; value: number }[];
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

  const shaped = shapeSeries(series, variant, start, end, step, additive);
  const values = shaped.series.flatMap((s) => s.points.map((p) => p[1])).filter((v): v is number => v !== null);
  const limits = thresholds.map((t) => t.value);
  const ticks = niceTicks(min ?? Math.min(0, ...values, ...limits), max ?? Math.max(...values, ...limits, 1), 3);
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
          {thresholds.map((t) => (
            <line
              key={t.severity}
              x1={left}
              x2={right}
              y1={y(t.value)}
              y2={y(t.value)}
              stroke={t.severity === "critical" ? "var(--threshold-crit)" : "var(--threshold-warn)"}
              strokeWidth={2}
              strokeDasharray="6 4"
            />
          ))}
          <ChartMarks
            series={shaped.series}
            variant={variant}
            step={shaped.step}
            x={x}
            y={y}
            low={low}
            right={right}
            color={color}
            lineClassName={styles.line}
          />
        </svg>
      </div>
      {legend && <ul className={styles.legend}>
        {series.map((s, i) => {
          const value = last(s.points);
          return (
            <li key={s.label}>
              <i className={styles.swatch} style={{ background: color(i) }} aria-hidden="true" />
              {s.label} <b>{value === null ? "–" : format(value)}</b>
            </li>
          );
        })}
      </ul>}
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
  const format = panelFormat(panel);
  if (panel.kind === "text") {
    return (
      <Frame panel={panel}>
        <Markdown text={panel.text ?? ""} />
      </Frame>
    );
  }
  if (!data || !window) {
    return (
      <Frame panel={panel}>
        <SkeletonLines label={t("display.screen.loading")} />
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
      if (panel.kind === "heatmap") {
        const { min, max } = panelScale(panel, data.series.flatMap((s) => s.points.map((p) => p[1])));
        return (
          <Frame panel={panel}>
            {data.series.length === 0 ? (
              <p className={styles.muted}>{t("display.screen.noData")}</p>
            ) : (
              <Heatmap
                label={panel.title}
                rows={worstFirst(data.series, panel.metric ?? "").map((s) => ({ key: s.label, name: s.label, label: s.label, points: s.points }))}
                start={window.start}
                end={window.end}
                step={window.step}
                format={format}
                color={heatColor(panel, min, max)}
              />
            )}
          </Frame>
        );
      }
      return (
        <Frame panel={panel}>
          {data.series.some((s) => s.points.some((p) => p[1] !== null)) ? (
            <ScreenChart
              series={data.series.map((s) => ({ label: seriesLabel(panel, s), points: s.points }))}
              start={window.start}
              end={window.end}
              step={window.step}
              format={format}
              {...chartBounds(panel, data.series.length)}
              variant={variantOf(panel)}
              additive={panelAdditive(panel)}
              color={(i) => panelColor(panel, i)}
              legend={panel.legend ?? true}
              thresholds={chartThresholds(panel)}
            />
          ) : (
            <p className={styles.muted}>{t("display.screen.noData")}</p>
          )}
        </Frame>
      );
    case "figure":
      if (panel.kind === "gauge") {
        const { min, max } = panelScale(panel, [data.value]);
        return (
          <Frame panel={panel}>
            <div className={styles.dial}>
              <Gauge
                value={data.value}
                min={min}
                max={max}
                format={format}
                color={levelColor(panel, data.value)}
                bands={thresholdBands(panel, min, max)}
                label={`${panel.title}: ${data.value === null ? t("display.screen.noValue") : format(data.value)}`}
              />
            </div>
          </Frame>
        );
      }
      return (
        <Frame panel={panel}>
          <p className={styles.figure} style={panel.thresholds && data.value !== null ? { color: levelColor(panel, data.value) } : undefined}>
            {data.value === null ? "–" : format(data.value)}
          </p>
          <p className={styles.muted}>
            {data.value === null ? t("display.screen.noValue") : t("display.panel.servers", { count: data.servers })}
          </p>
        </Frame>
      );
    case "top":
      return (
        <Frame panel={panel}>
          {data.rows.length === 0 ? (
            <p className={styles.muted}>{t("display.screen.noValues")}</p>
          ) : panel.kind === "bars" ? (
            <BarList
              label={panel.title}
              {...panelScale(panel, data.rows.map((r) => r.value))}
              format={format}
              bars={data.rows.map((row) => ({
                key: `${row.server_id}/${row.instance ?? ""}`,
                value: row.value,
                color: levelColor(panel, row.value),
                label: row.instance && !panel.instance ? `${row.hostname} ${row.instance}` : row.hostname,
              }))}
            />
          ) : (
            <ol className={styles.rows}>
              {data.rows.map((row) => (
                <li key={`${row.server_id}/${row.instance ?? ""}`}>
                  <span className={styles.name}>
                    {row.hostname}
                    {/* The instance the panel watches ("total") is the same on every row. */}
                    {row.instance && !panel.instance && <span className={styles.muted}> {row.instance}</span>}
                  </span>
                  <b>{format(row.value ?? Number.NaN)}</b>
                </li>
              ))}
            </ol>
          )}
        </Frame>
      );
    case "table": {
      const columns = panel.columns ?? [];
      return (
        <Frame panel={panel}>
          {data.rows.length === 0 ? (
            <p className={styles.muted}>{t("display.screen.noValues")}</p>
          ) : (
            <table className={styles.table}>
              <thead>
                <tr>
                  <th>{t("dashboards.table.server")}</th>
                  {columns.map((c, i) => (
                    <th key={i}>{columnTitle(c)}</th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {data.rows.map((row) => (
                  <tr key={row.server_id}>
                    <th>{row.hostname}</th>
                    {columns.map((c, i) => (
                      <td key={i}>{formatSensorValue(c.metric, row.values[i] ?? null)}</td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </Frame>
      );
    }
    case "text":
      return null;
    case "alerts": {
      const now = new Date(window.at * 1000);
      return (
        <Frame panel={panel}>
          {data.alerts.length === 0 && !data.unreachable?.length ? (
            <p className={styles.ok}>{t("display.screen.noAlerts")}</p>
          ) : (
            <ul className={styles.rows}>
              {data.alerts.map((alert) => (
                <li key={alert.id} className={alert.severity === "critical" ? styles.critical : styles.warning}>
                  <span className={styles.name}>
                    <b>{alert.severity === "critical" ? t("display.panel.critical") : t("display.panel.warning")}</b>{" "}
                    {incidentTitle(alert)}{" "}
                    <span className={styles.muted}>{t("display.panel.on", { hostname: alert.hostname })}</span>
                  </span>
                  <span className={styles.muted}>
                    {t("display.panel.since", { time: formatSince(new Date(alert.opened_at), now) })}
                    {alert.silenced_until && ` · ${quiet(alert, now)}`}
                  </span>
                </li>
              ))}
              {data.unreachable?.map((server) => (
                <li key={server.server_id}>
                  <span className={styles.name}>
                    <b>{t("common.state.unreachable")}</b> {server.hostname}
                  </span>
                  <span className={styles.muted}>{t("dependencies.fleet.because", { parent: endWords(server.because) })}</span>
                </li>
              ))}
            </ul>
          )}
        </Frame>
      );
    }
  }
}
