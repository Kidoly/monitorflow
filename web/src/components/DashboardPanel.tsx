"use client";

import Link from "next/link";
import type { ReactNode } from "react";

import { t } from "@/i18n";
import type { ChartLine, DashboardData, Panel, PanelAlert, PanelData } from "@/lib/api";
import { endWords } from "@/lib/dependencies";
import {
  heatColor,
  levelColor, panelAdditive,
  panelColor,
  panelFormat,
  panelScale,
  panelUnit,
  stateColor,
  thresholdBands,
  worstFirst,
} from "@/lib/dashboards";
import { formatValue } from "@/lib/format";
import { metricTitle } from "@/lib/metrics";
import { formatSensorValue } from "@/lib/sensors";
import { incidentTitle } from "@/lib/incidents";
import { incidentHref, sensorHref, serverHref } from "@/lib/routes";
import { type Scope, scopedHref } from "@/lib/scope";
import { BarList } from "@/components/ui/BarList";
import { stacks } from "@/components/ui/ChartMarks";
import { Gauge } from "@/components/ui/Gauge";
import { Donut } from "@/components/ui/Donut";
import { Heatmap } from "@/components/ui/Heatmap";
import { StateGrid } from "@/components/ui/StateGrid";
import { Timeline } from "@/components/ui/Timeline";
import { Markdown } from "@/components/ui/Markdown";
import { Label } from "@/components/ui/StatusLabel";
import { type Threshold, TimeChart } from "@/components/ui/TimeChart";
import { formatClock, formatDay, formatSince } from "@/components/ui/time";

import styles from "./Dashboards.module.css";
import { LiveMap } from "./MapCanvas";
import mapStyles from "./Maps.module.css";

/**
 * A chart line's name: the server (the API's label), or for servers
 * combined into one line, how they combine in the active language; the
 * API's label for it is English.
 */
export function seriesLabel(panel: Panel, line: ChartLine): string {
  if (line.server_id) return line.label;
  return panel.combine && panel.combine !== "each" ? t(`dashboards.combine.${panel.combine}`) : line.label;
}

/** How a chart over time draws its series. */
export const variantOf = (panel: Panel) => (panel.kind === "area" ? "area" : panel.kind === "columns" ? "columns" : "line");

/** The panel's thresholds as dashed lines of a chart. */
export function chartThresholds(panel: Panel): Threshold[] {
  const { warning, critical } = panel.thresholds ?? {};
  return [
    ...(warning !== undefined ? [{ severity: "warning" as const, value: warning, label: t("display.panel.warning") }] : []),
    ...(critical !== undefined ? [{ severity: "critical" as const, value: critical, label: t("display.panel.critical") }] : []),
  ];
}

/**
 * The y bounds of a chart: the panel's, else 0 to 100 % for ratios unless
 * stacked series add up beyond it.
 */
export function chartBounds(panel: Panel, lines: number): { min?: number; max?: number } {
  const ratio = panelUnit(panel) === "ratio" && !stacks(variantOf(panel), lines, panelAdditive(panel));
  return { min: panel.min ?? (ratio ? 0 : undefined), max: panel.max ?? (ratio ? 1 : undefined) };
}

/** Why a panel shows nothing, in plain words; null when it shows something. */
/** "in maintenance until 04:00 today", "silenced until …": why nobody is told of an alert (ADR-0151). */
export function quiet(alert: PanelAlert, now: Date): string {
  const when = formatDay(new Date(alert.silenced_until ?? 0), now);
  return alert.in_maintenance ? t("display.panel.inMaintenance", { when }) : t("display.panel.silenced", { when });
}

type Data<K extends PanelData["kind"]> = Extract<PanelData, { kind: K }>;

/** What a states panel's squares and key show, on a page or a screen. */
export function stateSquares(panel: Panel, data: Data<"states">, href?: (serverId: string) => string) {
  const format = panelFormat(panel);
  if (panel.metric) {
    const { min, max } = panelScale(panel, data.cells.map((c) => c.value ?? null));
    const colour = heatColor(panel, min, max);
    return {
      squares: data.cells.map((c) => {
        const value = c.value ?? null;
        const shade = value === null ? { background: "var(--border)", opacity: 1 } : colour(value);
        return {
          key: c.server_id,
          name: `${c.hostname}: ${value === null ? t("display.panel.noValue") : format(value)}`,
          color: shade.background,
          opacity: shade.opacity,
          href: href?.(c.server_id),
        };
      }),
      legend: [],
    };
  }
  const counts = new Map<string, number>();
  for (const c of data.cells) if (c.state) counts.set(c.state, (counts.get(c.state) ?? 0) + 1);
  return {
    squares: data.cells.map((c) => ({
      key: c.server_id,
      name: `${c.hostname}: ${c.state ? t(`common.state.${c.state}`) : ""}`,
      color: c.state ? stateColor(c.state) : "var(--border)",
      href: href?.(c.server_id),
    })),
    legend: [...counts].map(([state, count]) => ({
      key: state,
      label: `${count} ${t(`common.state.${state as NonNullable<(typeof data.cells)[number]["state"]>}`)}`,
      color: stateColor(state as NonNullable<(typeof data.cells)[number]["state"]>),
    })),
  };
}

/** A pie's slices, coloured by state or along the panel's palette, and the text in its middle. */
export function pieSlices(panel: Panel, data: Data<"pie">) {
  const format = panel.metric ? panelFormat(panel) : (v: number) => String(v);
  const slices = data.slices.map((s, i) => ({
    key: s.state ?? s.server_id ?? "other",
    label: s.state ? t(`common.state.${s.state}`) : s.server_id ? s.label : t("dashboards.pie.other"),
    value: s.value,
    color: s.state ? stateColor(s.state) : s.server_id ? panelColor(panel, i) : "var(--border-strong)",
  }));
  const sum = data.slices.reduce((a, s) => a + s.value, 0);
  return { slices, format, total: panel.metric ? format(sum) : String(sum) };
}

/** The ticks under a timeline: its start, middle and end. */
export function timelineTicks(start: number, end: number): [string, string, string] {
  const label = (t: number) => {
    const d = new Date(t * 1000);
    return end - start > 86_400 ? formatDay(d, new Date(end * 1000)) : formatClock(d).slice(0, 5);
  };
  return [label(start), label((start + end) / 2), label(end)];
}

/** "99.2 % without alerts". */
export const clearShare = (share: number) =>
  t("dashboards.timeline.clear", { share: formatValue(share, "ratio") });

/** A table column's heading: the metric's title, and the instance it watches. */
export function columnTitle(column: { metric: string; instance?: string }): string {
  const title = metricTitle(column.metric);
  return column.instance && column.instance !== "total" ? `${title} ${column.instance}` : title;
}

export function emptyReason(panel: Panel, data: PanelData): string | null {
  if (data.servers > 0 || data.kind === "text" || data.kind === "map") return null;
  if (data.unavailable > 0 || panel.servers.kind !== "all") {
    return t("display.panel.unavailable");
  }
  return t("display.panel.noServers");
}

function Frame({ panel, children, note }: { panel: Panel; children: ReactNode; note?: ReactNode }) {
  return (
    <section className={styles.box} aria-label={panel.title}>
      <h2 className={styles.boxTitle}>{panel.title}</h2>
      {children}
      {note && <p className={styles.note}>{note}</p>}
    </section>
  );
}

/**
 * One panel of a dashboard with its data: a chart, a figure, a top list or
 * open alerts. `data` undefined is still loading.
 */
export function DashboardPanel({
  panel,
  data,
  window,
  scope,
}: {
  panel: Panel;
  data: PanelData | undefined;
  window: Pick<DashboardData, "start" | "end" | "step" | "at"> | undefined;
  scope: Scope;
}) {
  const metric = panel.metric ?? "";
  const format = panelFormat(panel);
  const missing =
    data && data.unavailable > 0 && data.servers > 0
      ? t("display.panel.missing", { count: data.unavailable })
      : undefined;

  // Text reads nothing: it shows without waiting for the data.
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
        <div className={styles.loading} aria-busy="true" role="status" aria-label={t("display.panel.loading", { title: panel.title })} />
      </Frame>
    );
  }
  if (data.kind === "error") {
    return (
      <Frame panel={panel}>
        <p className={styles.error} role="alert">
          {data.error}
        </p>
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
    case "chart": {
      if (panel.kind === "heatmap") {
        const { min, max } = panelScale(panel, data.series.flatMap((s) => s.points.map((p) => p[1])));
        return (
          <Frame panel={panel} note={missing}>
            {data.series.length === 0 ? (
              <p className={styles.muted}>{t("display.panel.noData")}</p>
            ) : (
              <Heatmap
                label={panel.title}
                rows={worstFirst(data.series, metric).map((s) => ({
                  key: s.label,
                  name: s.label,
                  points: s.points,
                  label: s.server_id ? <Link href={scopedHref(serverHref(s.server_id), scope)}>{s.label}</Link> : s.label,
                }))}
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
      // "One line per server" draws the worst few: say how many of them.
      const drawn = new Set(data.series.flatMap((s) => (s.server_id ? [s.server_id] : []))).size;
      const subtitle =
        panel.combine === "each" && drawn < data.servers
          ? t("display.panel.someServers", { shown: drawn, count: data.servers })
          : t("display.panel.servers", { count: data.servers });
      return (
        <div className={styles.chart}>
          <TimeChart
            title={panel.title}
            subtitle={subtitle}
            series={data.series.map((s) => ({ label: seriesLabel(panel, s), points: s.points }))}
            start={window.start}
            end={window.end}
            step={window.step}
            format={format}
            {...chartBounds(panel, data.series.length)}
            thresholds={chartThresholds(panel)}
            variant={variantOf(panel)}
            additive={panelAdditive(panel)}
            legend={panel.legend ?? true}
            color={(i) => panelColor(panel, i)}
            emptyText={t("display.panel.noData")}
          />
          {missing && <p className={styles.note}>{missing}</p>}
        </div>
      );
    }
    case "figure": {
      if (panel.kind === "gauge") {
        const { min, max } = panelScale(panel, [data.value]);
        return (
          <Frame panel={panel} note={missing}>
            <div className={styles.gauge}>
              <Gauge
                value={data.value}
                min={min}
                max={max}
                format={format}
                color={levelColor(panel, data.value)}
                bands={thresholdBands(panel, min, max)}
                label={`${panel.title}: ${data.value === null ? t("display.panel.noValue") : format(data.value)}`}
              />
            </div>
            {data.value === null && <p className={styles.muted}>{t("display.panel.noValue")}</p>}
          </Frame>
        );
      }
      return (
        <Frame panel={panel} note={missing}>
          {/* Coloured only past a threshold: a plain figure stays in the text colour. */}
          <p className={styles.figure} style={panel.thresholds && data.value !== null ? { color: levelColor(panel, data.value) } : undefined}>
            {data.value === null ? "–" : format(data.value)}
          </p>
          {/* With a value, no "Now, of 16 servers" under each figure: it said the same on every card. */}
          {data.value === null && <p className={styles.muted}>{t("display.panel.noValue")}</p>}
        </Frame>
      );
    }
    case "top":
      return (
        <Frame panel={panel} note={missing}>
          {data.rows.length === 0 ? (
            <p className={styles.muted}>{t("display.panel.noValues")}</p>
          ) : panel.kind === "bars" ? (
            <BarList
              label={panel.title}
              {...panelScale(panel, data.rows.map((r) => r.value))}
              format={format}
              bars={data.rows.map((row) => ({
                key: `${row.server_id}/${row.instance ?? ""}`,
                value: row.value,
                color: levelColor(panel, row.value),
                label: (
                  <>
                    <Link href={scopedHref(sensorHref(row.server_id, metric, row.instance ?? ""), scope)}>{row.hostname}</Link>
                    {row.instance && !panel.instance && <span className={styles.muted}> {row.instance}</span>}
                  </>
                ),
              }))}
            />
          ) : (
            <ol className={styles.rows}>
              {data.rows.map((row) => (
                <li key={`${row.server_id}/${row.instance ?? ""}`} className={styles.row}>
                  <Link href={scopedHref(sensorHref(row.server_id, metric, row.instance ?? ""), scope)}>
                    {row.hostname}
                  </Link>
                  {/* The instance the panel watches ("total") is the same on every row. */}
                  {row.instance && !panel.instance && <span className={styles.muted}> {row.instance}</span>}
                  <span className={styles.value}>{format(row.value ?? Number.NaN)}</span>
                </li>
              ))}
            </ol>
          )}
        </Frame>
      );
    case "table": {
      const columns = panel.columns ?? [];
      return (
        <Frame panel={panel} note={missing}>
          {data.rows.length === 0 ? (
            <p className={styles.muted}>{t("display.panel.noValues")}</p>
          ) : (
            <div className={styles.tableBox}>
              <table className={styles.table}>
                <thead>
                  <tr>
                    <th scope="col">{t("dashboards.table.server")}</th>
                    {columns.map((c, i) => (
                      <th key={i} scope="col">
                        {columnTitle(c)}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {data.rows.map((row) => (
                    <tr key={row.server_id}>
                      <th scope="row">
                        <Link href={scopedHref(serverHref(row.server_id), scope)}>{row.hostname}</Link>
                      </th>
                      {columns.map((c, i) => (
                        <td key={i}>{formatSensorValue(c.metric, row.values[i] ?? null)}</td>
                      ))}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </Frame>
      );
    }
    case "text":
      return null;
    case "map":
      return (
        <Frame panel={panel}>
          <div className={mapStyles.panel}>
            <LiveMap live={data.map} mode="panel" />
          </div>
        </Frame>
      );
    case "states": {
      const { squares, legend } = stateSquares(panel, data, (id) => scopedHref(serverHref(id), scope));
      return (
        <Frame panel={panel} note={missing}>
          {squares.length === 0 ? (
            <p className={styles.muted}>{t("display.panel.noValues")}</p>
          ) : (
            <StateGrid label={panel.title} squares={squares} legend={legend} />
          )}
        </Frame>
      );
    }
    case "pie": {
      const { slices, format: write, total } = pieSlices(panel, data);
      return (
        <Frame panel={panel} note={missing}>
          {slices.length === 0 ? (
            <p className={styles.muted}>{t("display.panel.noValues")}</p>
          ) : (
            <Donut label={panel.title} slices={slices} total={total} format={write} />
          )}
        </Frame>
      );
    }
    case "timeline": {
      const end = Math.max(window.end, window.at);
      return (
        <Frame panel={panel} note={missing}>
          <Timeline
            label={panel.title}
            start={window.start}
            end={end}
            share={clearShare}
            ticks={timelineTicks(window.start, end)}
            rows={data.rows.map((row) => ({
              key: row.server_id,
              label: <Link href={scopedHref(serverHref(row.server_id), scope)}>{row.hostname}</Link>,
              spans: row.spans,
            }))}
          />
        </Frame>
      );
    }
    case "alerts": {
      const now = new Date(window.at * 1000);
      return (
        <Frame panel={panel} note={missing}>
          {data.alerts.length === 0 && !data.unreachable?.length ? (
            <p className={styles.muted}>{t("display.panel.noAlerts")}</p>
          ) : (
            <ul className={styles.rows}>
              {data.alerts.map((alert) => (
                <li key={alert.id} className={styles.row}>
                  <Label tone={alert.severity === "critical" ? "bad" : "warn"}>
                    {alert.severity === "critical" ? t("display.panel.critical") : t("display.panel.warning")}
                  </Label>
                  <Link href={scopedHref(incidentHref(alert.id), scope)}>
                    {incidentTitle({ metric: alert.metric, instance: alert.instance })}
                  </Link>
                  <span className={styles.muted}>{t("display.panel.on", { hostname: alert.hostname })}</span>
                  <span className={styles.value}>
                    {t("display.panel.since", { time: formatSince(new Date(alert.opened_at), now) })}
                  </span>
                  {alert.silenced_until && <span className={styles.muted}>{quiet(alert, now)}</span>}
                </li>
              ))}
              {data.unreachable?.map((server) => (
                <li key={server.server_id} className={styles.row}>
                  <Label tone="neutral">{t("common.state.unreachable")}</Label>
                  <Link href={scopedHref(serverHref(server.server_id), scope)}>{server.hostname}</Link>
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
