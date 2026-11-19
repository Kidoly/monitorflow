"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type EffectiveSensor,
  type EffectiveSensors,
  type EffectiveThreshold,
  type Incident,
  type MetricSeries,
  type SensorConfigEntry,
  type SensorValueList,
  type ServerDetail,
  api,
  describeError,
  isUnauthorized,
  isAbortError,
} from "@/lib/api";
import { RANGES, type Range, chartWindow, metricsError, seriesCsv } from "@/lib/charts";
import { diskIoCharts } from "@/lib/diskio";
import { formatSeconds, unitOf } from "@/lib/format";
import { every } from "@/lib/intervals";
import { modifiesSite, useMe } from "@/lib/me";
import { DEFAULT_INTERVAL_S, METRIC_PREFIX } from "@/lib/metrics";
import { incidentHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { type SensorStatus, formatSensorValue, primaryMetric, sensorOf, seriesTitle } from "@/lib/sensors";
import { incidentValue } from "@/lib/rules";
import { thresholdFor } from "@/lib/thresholds";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { Page } from "@/components/ui/Page";
import { Banner, ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { StatTiles } from "@/components/ui/StatTiles";
import { TableBox } from "@/components/ui/TableBox";
import { TimeChart } from "@/components/ui/TimeChart";
import { formatClock, formatDay, formatDuration } from "@/components/ui/time";
import type { Tone } from "@/components/ui/tone";

import { sensorSiblings, serverSiblings } from "./breadcrumbSiblings";
import { SensorConfiguration } from "./SensorConfiguration";
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
  /** The server's own sensor rows and what its site runs, to change the interval; null when unreadable. */
  rows: SensorConfigEntry[] | null;
  siteSensors: EffectiveSensors["sensors"] | null;
  thresholds: EffectiveThreshold[] | null;
  /** Why the sensor's last sample is late or missing, in the server's words; null when it is not. */
  late: string | null;
  /** Every sensor's recent values, for the breadcrumb's list; null when unreadable. */
  recent: SensorValueList | null;
  /** Every incident of this metric and instance, newest first; null when they could not be read. */
  incidents: Incident[] | null;
  chart: Chart<MetricSeries>;
  /** More charts of the same instance: a disk's activity (ADR-0092). */
  companions: Companion[];
  range: Range;
}

type Chart<S> = { start: number; end: number; step: number } & ({ series: S[] } | { error: string });

interface Companion {
  title: string;
  /** The metric of the first series, which gives the unit. */
  metric: string;
  chart: Chart<{ label: string; points: MetricSeries["points"] }>;
}

const orNull = <T,>(promise: Promise<T>) =>
  promise.catch((e: unknown) => {
    if (isAbortError(e)) throw e;
    return null;
  });

async function loadSensor(id: string, metric: string, instance: string, range: Range, signal: AbortSignal): Promise<SensorData> {
  const [server, config, rows, thresholds, incidents, recent] = await Promise.all([
    api.server(id, signal),
    orNull(api.serverSensors(id, signal)),
    orNull(api.sensorRows({ kind: "servers", id }, signal).then((r) => r.entries)),
    orNull(api.serverThresholds(id, signal).then((t) => t.thresholds)),
    orNull(api.incidents({ server_id: id, limit: 500 }, signal).then((p) => p.incidents)),
    orNull(api.sensorValues(id, signal)),
  ]);
  const siteSensors = config ? await orNull(api.siteSensors(config.site_id, signal).then((s) => s.sensors)) : null;
  const effective = config ? sensorOf(config.sensors, metric) : undefined;
  const intervalS = (effective?.interval_ms ?? DEFAULT_INTERVAL_S * 1000) / 1000;
  const others = effective?.kind === "diskio" ? diskIoCharts(effective.name, metric) : [];
  const [chart, companions] = await Promise.all([
    rangeChart(id, metric, instance, range, intervalS, signal),
    Promise.all(others.map((c) => companion(id, instance, range, intervalS, c, signal))),
  ]);
  return {
    server,
    config,
    rows,
    siteSensors,
    thresholds,
    incidents: incidents?.filter((i) => i.metric === metric && i.instance === instance) ?? null,
    late: lateReason(recent, effective, metric, instance),
    recent,
    chart,
    companions,
    range,
  };
}

/** The server's sentence for late or missing data of the sensor's own metric, if any. */
function lateReason(list: SensorValueList | null, sensor: EffectiveSensor | undefined, metric: string, instance: string): string | null {
  const entry = sensor && list?.sensors.find((v) => v.name === sensor.name && v.instance === sensor.instance && v.metric === metric);
  if (!entry) return null;
  const shown = entry.series.find((s) => s.instance === (instance || primaryMetric(sensor).instance || ""));
  return shown ? shown.reason : entry.series.length === 0 ? entry.reason : null;
}

/** `metric` of `instance` (every instance when empty) over `range`, or why it could not be read. */
async function rangeChart(
  id: string,
  metric: string,
  instance: string,
  range: Range,
  intervalS: number,
  signal: AbortSignal,
): Promise<Chart<MetricSeries>> {
  const window = chartWindow(range, metric, intervalS, Date.now() / 1000);
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
    return { ...window, series };
  } catch (e) {
    if (isAbortError(e)) throw e;
    return { ...window, error: metricsError(e) };
  }
}

/** One more chart: its metrics side by side, labelled. */
async function companion(
  id: string,
  instance: string,
  range: Range,
  intervalS: number,
  chart: ReturnType<typeof diskIoCharts>[number],
  signal: AbortSignal,
): Promise<Companion> {
  const parts = await Promise.all(chart.series.map((s) => rangeChart(id, s.metric, instance, range, intervalS, signal)));
  const { start, end, step } = parts[0]!;
  const failed = parts.find((p) => "error" in p);
  const series = parts.flatMap((part, i) =>
    "series" in part
      ? part.series.map((s) => ({
          label: instance ? chart.series[i]!.label : `${chart.series[i]!.label} ${s.labels.instance ?? ""}`.trim(),
          points: s.points,
        }))
      : [],
  );
  return {
    title: chart.title,
    metric: chart.series[0]!.metric,
    chart: failed && "error" in failed ? { start, end, step, error: failed.error } : { start, end, step, series },
  };
}

const TONES: Record<SensorStatus, Tone> = { critical: "bad", warning: "warn", unknown: "neutral", ok: "ok" };

export function SensorPage({ id, sensor, instance }: { id: string | null; sensor: string | null; instance: string }) {
  const valid = id !== null && UUID.test(id) && sensor !== null && SENSOR.test(sensor);
  const metric = `${METRIC_PREFIX}${sensor}`;
  const scope = useScope();
  const me = useMe();
  const [params, setParams] = useUrlParams();
  const range = RANGES.find((r) => r.label.replace(" ", "") === params.get("range")) ?? RANGES[1];
  const fetcher = useCallback(
    (signal: AbortSignal) => loadSensor(id ?? "", metric, instance, range, signal),
    [id, metric, instance, range],
  );
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });

  const effective = data?.config ? sensorOf(data.config.sensors, metric) : undefined;
  const title = seriesTitle(effective, metric, instance);
  const documentTitle = data ? t("sensors.page.documentTitle", { title, host: data.server.hostname }) : null;
  useEffect(() => {
    if (documentTitle) document.title = documentTitle;
  }, [documentTitle]);

  if (!valid || (error instanceof ApiError && error.status === 404)) {
    return (
      <NotFound
        title={t("sensors.page.notFound")}
        actions={<ButtonLink href={scopedHref("/", scope)}>{t("sensors.page.backToFleet")}</ButtonLink>}
      />
    );
  }
  if (!data || !updatedAt) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what={t("sensors.page.load")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("sensors.page.loading")} />
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
  const status: SensorStatus = open ? open.severity : current === null || data.late ? "unknown" : "ok";
  const rule = thresholds ? thresholdFor(thresholds, metric, instance) : undefined;
  const ratio = unitOf(metric) === "ratio";
  const serverLink = scopedHref(serverHref(server.id), scope);

  return (
    <Page>
      <Breadcrumb
        parents={[
          { label: t("sensors.page.fleet"), href: scopedHref("/", scope) },
          { label: server.hostname, href: serverLink, siblings: serverSiblings(server, scope) },
        ]}
        siblings={config ? sensorSiblings(server, config.sensors, data.recent, { metric, instance }, scope) : undefined}
      />
      <PageHeader
        detail
        title={title}
        tone={TONES[status]}
        reason={open ? open.reason : data.late}
        status={
          <>
            <StatusLabel kind="sensor" value={status} />
            <span className={styles.muted}>
              {t("sensors.page.on", { host: server.hostname })}
              {effective && t("sensors.page.kind", { kind: effective.kind, interval: formatSeconds(effective.interval_ms / 1000) })}
            </span>
          </>
        }
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={open && <ButtonLink href={scopedHref(incidentHref(open.id), scope)}>{t("sensors.page.openIncident")}</ButtonLink>}
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      <StatTiles
        compact
        label={t("sensors.page.summary")}
        tiles={[
          { label: t("sensors.page.current"), value: current === null ? "–" : format(current), tone: status === "ok" ? undefined : TONES[status] },
          { label: t("sensors.page.peak", { range: data.range.label }), value: values.length ? format(Math.max(...values)) : "–" },
          {
            label: t("sensors.page.average", { range: data.range.label }),
            value: values.length ? format(values.reduce((a, b) => a + b, 0) / values.length) : "–",
          },
          ...(open
            ? [{ label: t(`sensors.page.above.${open.severity}`), value: formatDuration((now.getTime() - Date.parse(open.opened_at)) / 1000) }]
            : []),
        ]}
      />
      {"error" in chart ? (
        <Banner tone="bad">{chart.error}</Banner>
      ) : (
        <TimeChart
          title={t("sensors.page.history")}
          subtitle={
            effective && effective.interval_ms > 60_000
              ? t("sensors.page.subtitleChecked", {
                  from: formatClock(new Date(chart.start * 1000)).slice(0, 5),
                  to: formatClock(new Date(chart.end * 1000)).slice(0, 5),
                  every: every(effective.interval_ms),
                })
              : t("sensors.page.subtitle", {
                  from: formatClock(new Date(chart.start * 1000)).slice(0, 5),
                  to: formatClock(new Date(chart.end * 1000)).slice(0, 5),
                })
          }
          actions={
            <div role="group" aria-label={t("sensors.page.range")} className={styles.segments}>
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
            return value === null || value === undefined ? [] : [{ severity, value, label: t(`sensors.severity.${severity}`) }];
          })}
          markers={open ? [{ t: Date.parse(open.opened_at) / 1000, label: t("sensors.page.opened", { time: formatClock(new Date(open.opened_at)).slice(0, 5) }), kind: "opened" }] : []}
          band={open && { start: Date.parse(open.opened_at) / 1000, severity: open.severity }}
          emptyText={t("sensors.page.empty")}
          footer={
            series.length > 0 && (
              <button type="button" className={styles.linkButton} onClick={() => download(title, server.hostname, series)}>
                {t("sensors.page.csv")}
              </button>
            )
          }
        />
      )}
      {data.companions.map((c) => (
        <CompanionChart key={c.title} companion={c} />
      ))}
      <div className={styles.pair}>
        <SensorConfiguration
          data={data}
          sensor={effective}
          metric={metric}
          instance={instance}
          format={format}
          canEdit={!server.revoked_at && modifiesSite(me, server.customer_id, server.site_id)}
          onChanged={refresh}
          startEditing={params.get("edit") === "1"}
          onEditEnd={() => setParams({ edit: null })}
        />
        <TableBox
          title={t("sensors.page.incidents")}
          meta={
            incidents &&
            t("sensors.page.incidentsMeta", {
              open: incidents.filter((i) => i.state === "open").length,
              month: incidents.filter((i) => now.getTime() - Date.parse(i.opened_at) <= 30 * DAY).length,
            })
          }
        >
          {incidents === null ? (
            <p>{t("sensors.page.incidentsFailed")}</p>
          ) : incidents.length === 0 ? (
            <p>{t("sensors.page.noIncidents")}</p>
          ) : (
            <SensorIncidents incidents={incidents} now={now} format={format} />
          )}
        </TableBox>
      </div>
    </Page>
  );
}

function CompanionChart({ companion }: { companion: Companion }) {
  const { title, metric, chart } = companion;
  if ("error" in chart) {
    return (
      <Banner tone="bad">
        {title}: {chart.error}
      </Banner>
    );
  }
  return (
    <TimeChart
      title={title}
      series={chart.series}
      start={chart.start}
      end={chart.end}
      step={chart.step}
      format={(v) => formatSensorValue(metric, v)}
      min={0}
      max={unitOf(metric) === "ratio" ? 1 : undefined}
      emptyText={t("sensors.page.emptyShort")}
    />
  );
}

function SensorIncidents({ incidents, now, format }: { incidents: Incident[]; now: Date; format: (v: number) => string }) {
  const scope = useScope();
  const columns: Column<Incident>[] = [
    {
      key: "severity",
      header: t("sensors.page.severity"),
      cell: (i) =>
        i.state === "open" ? (
          <>
            <StatusLabel kind="severity" value={i.severity} /> <span className={styles.small}>{t("sensors.page.openWord")}</span>
          </>
        ) : (
          <>
            <StatusLabel kind="severity" value="resolved" /> <span className={styles.small}>{t("sensors.page.was", { severity: t(`sensors.severity.${i.severity}`) })}</span>
          </>
        ),
    },
    {
      key: "opened",
      header: t("sensors.page.opened.column"),
      cell: (i) => (
        <Link href={scopedHref(incidentHref(i.id), scope)} className={i.state === "open" ? styles.strong : undefined}>
          {formatDay(new Date(i.opened_at), now)}
        </Link>
      ),
    },
    {
      key: "duration",
      header: t("sensors.page.duration"),
      cell: (i) => formatDuration(((i.resolved_at ? Date.parse(i.resolved_at) : now.getTime()) - Date.parse(i.opened_at)) / 1000),
    },
    // A forecast's peak is a time left, a silence's a duration (ADR-0153).
    { key: "peak", header: t("sensors.page.peakColumn"), cell: (i) => ((i.kind ?? "value") === "value" ? format(i.peak_value) : incidentValue(i, i.peak_value)) },
  ];
  return <DataTable bare dense label={t("sensors.page.incidents")} columns={columns} rows={incidents} rowKey={(i) => i.id} rowMuted={(i) => i.state === "resolved"} pageSize={10} />;
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
