"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect } from "react";

import {
  ApiError,
  type EffectiveSensors,
  type EffectiveThreshold,
  type Incident,
  type MetricSeries,
  type ServerDetail,
  api,
  describeError,
  isUnauthorized,
  isAbortError,
} from "@/lib/api";
import { RANGES, type Range, chartWindow, metricsError, seriesCsv } from "@/lib/charts";
import { formatSeconds, unitOf } from "@/lib/format";
import { DEFAULT_INTERVAL_S, METRIC_PREFIX } from "@/lib/metrics";
import { incidentHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { type SensorStatus, formatSensorValue, sensorOf, seriesTitle } from "@/lib/sensors";
import { thresholdFor } from "@/lib/thresholds";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { FactsBox } from "@/components/ui/FactsBox";
import { Page } from "@/components/ui/Page";
import { Banner, ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { StatTiles } from "@/components/ui/StatTiles";
import { TableBox } from "@/components/ui/TableBox";
import { TimeChart } from "@/components/ui/TimeChart";
import { formatClock, formatDay, formatDuration } from "@/components/ui/time";
import type { Tone } from "@/components/ui/tone";

import styles from "./DetailPages.module.css";

/** A metric name without the prefix, as in `?sensor=disk_used_ratio`. */
const SENSOR = /^[a-z0-9_]{1,200}$/;
const DAY = 86_400_000;

/** The sensor page for the `id`, `sensor` and `instance` query parameters. */
export function SensorView() {
  const params = useSearchParams();
  const id = params.get("id");
  const sensor = params.get("sensor");
  const instance = params.get("instance") ?? "";
  return <SensorPage key={`${id}/${sensor}/${instance}`} id={id} sensor={sensor} instance={instance} />;
}

interface SensorData {
  server: ServerDetail;
  config: EffectiveSensors | null;
  thresholds: EffectiveThreshold[] | null;
  /** Every incident of this metric and instance, newest first; null when they could not be read. */
  incidents: Incident[] | null;
  chart: { start: number; end: number; step: number } & ({ series: MetricSeries[] } | { error: string });
  range: Range;
}

const orNull = <T,>(promise: Promise<T>) =>
  promise.catch((e: unknown) => {
    if (isAbortError(e)) throw e;
    return null;
  });

async function loadSensor(id: string, metric: string, instance: string, range: Range, signal: AbortSignal): Promise<SensorData> {
  const [server, config, thresholds, incidents] = await Promise.all([
    api.server(id, signal),
    orNull(api.serverSensors(id, signal)),
    orNull(api.serverThresholds(id, signal).then((t) => t.thresholds)),
    orNull(api.incidents({ server_id: id, limit: 500 }, signal).then((p) => p.incidents)),
  ]);
  const interval = (config && sensorOf(config.sensors, metric)?.interval_ms) ?? DEFAULT_INTERVAL_S * 1000;
  const window = chartWindow(range, metric, interval / 1000, Date.now() / 1000);
  let chart: SensorData["chart"];
  try {
    const { series } = await api.queryRange(
      {
        serverId: id,
        metric,
        instance: instance || undefined,
        start: new Date(window.start * 1000),
        end: new Date(window.end * 1000),
        step: window.step,
        rollup: window.rollup,
      },
      signal,
    );
    chart = { ...window, series };
  } catch (e) {
    if (isAbortError(e)) throw e;
    chart = { ...window, error: metricsError(e) };
  }
  return {
    server,
    config,
    thresholds,
    incidents: incidents?.filter((i) => i.metric === metric && i.instance === instance) ?? null,
    chart,
    range,
  };
}

const TONES: Record<SensorStatus, Tone> = { critical: "bad", warning: "warn", unknown: "neutral", ok: "ok" };

export function SensorPage({ id, sensor, instance }: { id: string | null; sensor: string | null; instance: string }) {
  const valid = id !== null && UUID.test(id) && sensor !== null && SENSOR.test(sensor);
  const metric = `${METRIC_PREFIX}${sensor}`;
  const scope = useScope();
  const [params, setParams] = useUrlParams();
  const range = RANGES.find((r) => r.label.replace(" ", "") === params.get("range")) ?? RANGES[1];
  const fetcher = useCallback(
    (signal: AbortSignal) => loadSensor(id ?? "", metric, instance, range, signal),
    [id, metric, instance, range],
  );
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });

  const effective = data?.config ? sensorOf(data.config.sensors, metric) : undefined;
  const title = seriesTitle(effective, metric, instance);
  useEffect(() => {
    if (data) document.title = `${title} on ${data.server.hostname} · MonitorFlow`;
  }, [data, title]);

  if (!valid || (error instanceof ApiError && error.status === 404)) {
    return <NotFound title="Sensor not found" actions={<ButtonLink href={scopedHref("/", scope)}>Back to the fleet</ButtonLink>} />;
  }
  if (!data || !updatedAt) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what="the sensor" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading sensor" />
    );
  }

  const { server, config, thresholds, incidents, chart } = data;
  const now = updatedAt;
  const format = (v: number) => formatSensorValue(metric, v);
  const open = incidents?.find((i) => i.state === "open");
  const series = "series" in chart ? chart.series : [];
  const values = series.flatMap((s) => s.points.map((p) => p[1])).filter((v): v is number => v !== null);
  const lastPoints = series[0]?.points.filter((p) => p[1] !== null) ?? [];
  const current = lastPoints[lastPoints.length - 1]?.[1] ?? null;
  const status: SensorStatus = open ? open.severity : current === null ? "unknown" : "ok";
  const rule = thresholds ? thresholdFor(thresholds, metric, instance) : undefined;
  const ratio = unitOf(metric) === "ratio";
  const serverLink = scopedHref(serverHref(server.id), scope);

  return (
    <Page>
      <Breadcrumb
        parents={[
          { label: "Fleet", href: scopedHref("/", scope) },
          { label: server.hostname, href: serverLink },
        ]}
        current={title}
      />
      <PageHeader
        detail
        title={title}
        status={
          <>
            <StatusLabel kind="sensor" value={status} />
            <span className={styles.muted}>
              on {server.hostname}
              {effective && ` · ${effective.kind} sensor · every ${formatSeconds(effective.interval_ms / 1000)}`}
            </span>
          </>
        }
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={open && <ButtonLink href={scopedHref(incidentHref(open.id), scope)}>Open incident</ButtonLink>}
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      <StatTiles
        compact
        label="Summary"
        tiles={[
          { label: "Current", value: current === null ? "–" : format(current), tone: TONES[status] },
          { label: `Peak, ${data.range.label}`, value: values.length ? format(Math.max(...values)) : "–" },
          {
            label: `Average, ${data.range.label}`,
            value: values.length ? format(values.reduce((a, b) => a + b, 0) / values.length) : "–",
          },
          ...(open
            ? [{ label: `Above ${open.severity} for`, value: formatDuration((now.getTime() - Date.parse(open.opened_at)) / 1000) }]
            : []),
        ]}
      />
      {"error" in chart ? (
        <Banner tone="bad">{chart.error}</Banner>
      ) : (
        <TimeChart
          title="History"
          subtitle={`${formatClock(new Date(chart.start * 1000)).slice(0, 5)} → ${formatClock(new Date(chart.end * 1000)).slice(0, 5)} · hover or use ← → for exact values`}
          actions={
            <div role="group" aria-label="Time range" className={styles.segments}>
              {RANGES.map((r) => (
                <Button
                  key={r.label}
                  aria-pressed={r === range}
                  onClick={() => setParams({ range: r === RANGES[1] ? null : r.label.replace(" ", "") })}
                >
                  {r.label}
                </Button>
              ))}
            </div>
          }
          series={series.map((s) => ({ label: s.labels.instance ? seriesTitle(effective, metric, s.labels.instance) : title, points: s.points }))}
          start={chart.start}
          end={chart.end}
          step={chart.step}
          format={format}
          min={ratio ? 0 : undefined}
          max={ratio ? 1 : undefined}
          thresholds={(["warning", "critical"] as const).flatMap((severity) => {
            const value = rule?.[severity];
            return value === null || value === undefined ? [] : [{ severity, value, label: severity === "warning" ? "Warning" : "Critical" }];
          })}
          markers={open ? [{ t: Date.parse(open.opened_at) / 1000, label: `Opened ${formatClock(new Date(open.opened_at)).slice(0, 5)}`, kind: "opened" }] : []}
          band={open && { start: Date.parse(open.opened_at) / 1000, severity: open.severity }}
          emptyText="No data in this range yet. New points take about 30 seconds to appear."
          footer={
            series.length > 0 && (
              <button type="button" className={styles.linkButton} onClick={() => download(title, server.hostname, series)}>
                Download CSV
              </button>
            )
          }
        />
      )}
      <div className={styles.pair}>
        <FactsBox
          title="Configuration"
          facts={[
            { label: "Kind", value: effective?.kind ?? "unknown" },
            { label: "Instance", value: instance || effective?.instance || "—", mono: Boolean(instance || effective?.instance) },
            { label: "Interval", value: effective ? formatSeconds(effective.interval_ms / 1000) : "—" },
            {
              label: "Timeout",
              value: effective ? (effective.timeout_ms === null ? "agent default" : formatSeconds(effective.timeout_ms / 1000)) : "—",
            },
            ...thresholdFacts(rule, format, thresholds === null),
            {
              label: "Alerts",
              value: <Link href={scopedHref(serverHref(server.id, "alerts"), scope)}>Change when to alert</Link>,
            },
            ...(effective && Object.keys(effective.params).length > 0
              ? [{ label: "Parameters", value: Object.entries(effective.params).map(([k, v]) => `${k}=${v}`).join(" "), mono: true }]
              : []),
            ...(config && effective ? originFacts(config, effective.origin, rule) : []),
          ]}
        />
        <TableBox
          title="Incidents on this sensor"
          meta={
            incidents &&
            `${incidents.filter((i) => i.state === "open").length} open · ${incidents.filter((i) => now.getTime() - Date.parse(i.opened_at) <= 30 * DAY).length} in 30 days`
          }
        >
          {incidents === null ? (
            <p>The incidents could not be read. Retrying every 15 seconds.</p>
          ) : incidents.length === 0 ? (
            <p>No incidents on this sensor.</p>
          ) : (
            <SensorIncidents incidents={incidents} now={now} format={format} />
          )}
        </TableBox>
      </div>
    </Page>
  );
}

function thresholdFacts(rule: EffectiveThreshold | undefined, format: (v: number) => string, failed: boolean) {
  if (!rule) return [{ label: "Thresholds", value: failed ? "could not be read" : "none: this sensor never opens incidents" }];
  const above = rule.comparison === ">" || rule.comparison === ">=";
  return [
    { label: "Warning", value: rule.warning === null ? "none" : `${rule.comparison} ${format(rule.warning)}` },
    { label: "Critical", value: rule.critical === null ? "none" : `${rule.comparison} ${format(rule.critical)}` },
    { label: "Duration", value: <>{formatSeconds(rule.duration_s)} <span className={styles.muted}>past the threshold before opening</span></> },
    {
      label: "Hysteresis",
      value: (
        <>
          {format(rule.hysteresis)} <span className={styles.muted}>{above ? "below" : "above"} the threshold before resolving</span>
        </>
      ),
    },
  ];
}

const ORIGINS = { default: "Built-in default", site: "The site", server: "This server" } as const;

function originFacts(config: EffectiveSensors, origin: keyof typeof ORIGINS, rule: EffectiveThreshold | undefined) {
  return [
    {
      label: "Origin",
      value: (
        <>
          {ORIGINS[origin]}{" "}
          <span className={styles.muted}>
            · revision {config.revision}
            {rule && ` · thresholds from ${ORIGINS[rule.origin].toLowerCase()}`}
          </span>
        </>
      ),
    },
    {
      label: "Agent runs",
      value:
        config.applied_revision === null ? (
          <Label tone="neutral">Not applied</Label>
        ) : (
          <>
            <Label tone={config.in_sync ? "ok" : "warn"}>Revision {config.applied_revision}</Label>{" "}
            <span className={styles.muted}>{config.in_sync ? "in sync" : "configuration out of date"}</span>
          </>
        ),
    },
  ];
}

function SensorIncidents({ incidents, now, format }: { incidents: Incident[]; now: Date; format: (v: number) => string }) {
  const scope = useScope();
  const columns: Column<Incident>[] = [
    {
      key: "severity",
      header: "Severity",
      cell: (i) =>
        i.state === "open" ? (
          <>
            <StatusLabel kind="severity" value={i.severity} /> <span className={styles.small}>open</span>
          </>
        ) : (
          <>
            <StatusLabel kind="severity" value="resolved" /> <span className={styles.small}>was {i.severity === "critical" ? "Critical" : "Warning"}</span>
          </>
        ),
    },
    {
      key: "opened",
      header: "Opened",
      cell: (i) => (
        <Link href={scopedHref(incidentHref(i.id), scope)} className={i.state === "open" ? styles.strong : undefined}>
          {formatDay(new Date(i.opened_at), now)}
        </Link>
      ),
    },
    {
      key: "duration",
      header: "Duration",
      cell: (i) => formatDuration(((i.resolved_at ? Date.parse(i.resolved_at) : now.getTime()) - Date.parse(i.opened_at)) / 1000),
    },
    { key: "peak", header: "Peak", cell: (i) => format(i.peak_value) },
  ];
  return <DataTable bare dense label="Incidents on this sensor" columns={columns} rows={incidents} rowKey={(i) => i.id} rowMuted={(i) => i.state === "resolved"} pageSize={10} />;
}

function download(title: string, hostname: string, series: MetricSeries[]) {
  const csv = seriesCsv(series.map((s) => ({ label: s.labels.instance || title, points: s.points })));
  const url = URL.createObjectURL(new Blob([csv], { type: "text/csv" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = `${hostname}-${title.replace(/[^\w.-]+/g, "_")}.csv`;
  link.click();
  URL.revokeObjectURL(url);
}
