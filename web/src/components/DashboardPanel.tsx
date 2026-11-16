"use client";

import Link from "next/link";
import type { ReactNode } from "react";

import { t } from "@/i18n";
import type { DashboardData, Panel, PanelData } from "@/lib/api";
import { formatNumber, unitOf } from "@/lib/format";
import { incidentTitle } from "@/lib/incidents";
import { incidentHref, sensorHref } from "@/lib/routes";
import { type Scope, scopedHref } from "@/lib/scope";
import { formatSensorValue } from "@/lib/sensors";
import { Label } from "@/components/ui/StatusLabel";
import { TimeChart } from "@/components/ui/TimeChart";
import { formatSince } from "@/components/ui/time";

import styles from "./Dashboards.module.css";

/** Why a panel shows nothing, in plain words; null when it shows something. */
export function emptyReason(panel: Panel, data: PanelData): string | null {
  if (data.servers > 0) return null;
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
  const format = (v: number) => formatSensorValue(metric, v);
  const missing =
    data && data.unavailable > 0 && data.servers > 0
      ? t("display.panel.missing", { count: data.unavailable })
      : undefined;

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
      const ratio = unitOf(metric) === "ratio";
      return (
        <div className={styles.chart}>
          <TimeChart
            title={panel.title}
            subtitle={t("display.panel.servers", { count: data.servers })}
            series={data.series.map((s) => ({ label: s.label, points: s.points }))}
            start={window.start}
            end={window.end}
            step={window.step}
            format={format}
            min={ratio ? 0 : undefined}
            max={ratio ? 1 : undefined}
            emptyText={t("display.panel.noData")}
          />
          {missing && <p className={styles.note}>{missing}</p>}
        </div>
      );
    }
    case "figure":
      return (
        <Frame panel={panel} note={missing}>
          <p className={styles.figure}>
            {data.value === null
              ? "–"
              : panel.combine === "count"
                ? formatNumber(data.value)
                : format(data.value)}
          </p>
          <p className={styles.muted}>
            {data.value === null
              ? t("display.panel.noValue")
              : t("display.panel.now", { count: data.servers })}
          </p>
        </Frame>
      );
    case "top":
      return (
        <Frame panel={panel} note={missing}>
          {data.rows.length === 0 ? (
            <p className={styles.muted}>{t("display.panel.noValues")}</p>
          ) : (
            <ol className={styles.rows}>
              {data.rows.map((row) => (
                <li key={`${row.server_id}/${row.instance ?? ""}`} className={styles.row}>
                  <Link href={scopedHref(sensorHref(row.server_id, metric, row.instance ?? ""), scope)}>
                    {row.hostname}
                  </Link>
                  {row.instance && <span className={styles.muted}> {row.instance}</span>}
                  <span className={styles.value}>{format(row.value ?? Number.NaN)}</span>
                </li>
              ))}
            </ol>
          )}
        </Frame>
      );
    case "alerts": {
      const now = new Date(window.at * 1000);
      return (
        <Frame panel={panel} note={missing}>
          {data.alerts.length === 0 ? (
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
                </li>
              ))}
            </ul>
          )}
        </Frame>
      );
    }
  }
}
