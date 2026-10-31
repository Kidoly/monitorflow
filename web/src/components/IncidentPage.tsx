"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useRef } from "react";

import {
  ApiError,
  type IncidentDetail,
  type IncidentEvent,
  type MetricSeries,
  type RemediationIncident,
  type ThresholdSnapshot,
  api,
  describeError,
  isUnauthorized,
  isAbortError,
} from "@/lib/api";
import { metricsError } from "@/lib/charts";
import { formatSeconds } from "@/lib/format";
import { INCIDENT_MARGINS, OFFLINE_AFTER_S, incidentTitle, incidentWindow, isOffline, resolvesWhen } from "@/lib/incidents";
import { incidentPeerHref, incidentsHref, sensorHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { formatSensorValue, seriesTitle } from "@/lib/sensors";
import { SEVERITY_LABELS } from "@/lib/thresholds";
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
import { type Threshold, TimeChart } from "@/components/ui/TimeChart";
import { formatClock, formatDuration, formatExact, formatShortDate } from "@/components/ui/time";
import type { Tone } from "@/components/ui/tone";

import styles from "./DetailPages.module.css";

/** The incident page for the `id` query parameter. */
export function IncidentView() {
  const id = useSearchParams().get("id");
  return <IncidentPage key={id} id={id} />;
}

type Margin = (typeof INCIDENT_MARGINS)[number];
type Chart = { start: number; end: number; step: number } & ({ series: MetricSeries[] } | { error: string });

interface Loaded {
  incident: IncidentDetail;
  /** Null for an offline incident, which has no metric to draw. */
  chart: Chart | null;
  /** What the AI remediation loop did about it; null when nothing, or not readable. */
  remediation: RemediationIncident | null;
  margin: Margin;
}

async function loadIncident(id: string, margin: Margin, signal: AbortSignal): Promise<Loaded> {
  const [incident, remediation] = await Promise.all([
    api.incident(id, signal),
    api.remediationIncidents(id, signal).then(
      (page) => page.incidents[0] ?? null,
      (e: unknown) => {
        if (isAbortError(e)) throw e;
        return null;
      },
    ),
  ]);
  if (isOffline(incident) || !incident.server_id) return { incident, chart: null, remediation, margin };
  const window = incidentWindow(incident.opened_at, incident.resolved_at, new Date(), margin.seconds);
  let chart: Chart;
  try {
    const { series } = await api.queryRange(
      {
        serverId: incident.server_id,
        metric: incident.metric,
        // The series without an instance is the metric's only one.
        instance: incident.instance || undefined,
        start: new Date(window.start * 1000),
        end: new Date(window.end * 1000),
        step: window.step,
      },
      signal,
    );
    chart = { ...window, series };
  } catch (e) {
    if (isAbortError(e)) throw e;
    chart = { ...window, error: metricsError(e) };
  }
  return { incident, chart, remediation, margin };
}

const minutes = (iso: string) => formatClock(new Date(iso)).slice(0, 5);

export function IncidentPage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const scope = useScope();
  const [params, setParams] = useUrlParams();
  const margin = INCIDENT_MARGINS.find((m) => m.label.replace(/[± ]/g, "") === params.get("window")) ?? INCIDENT_MARGINS[0];
  // A resolved incident no longer changes: refreshes reuse it until the window changes.
  const last = useRef<Loaded | null>(null);
  const fetcher = useCallback(
    async (signal: AbortSignal) => {
      const previous = last.current;
      if (previous?.incident.state === "resolved" && previous.margin === margin) return previous;
      last.current = await loadIncident(id ?? "", margin, signal);
      return last.current;
    },
    [id, margin],
  );
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });

  const incident = data?.incident;
  const title = incident ? incidentTitle(incident) : "";
  useEffect(() => {
    if (incident) document.title = `${title} on ${incident.hostname} · MonitorFlow`;
  }, [incident, title]);

  if (!valid || (error instanceof ApiError && error.status === 404)) {
    return (
      <NotFound
        title="Incident not found"
        actions={<ButtonLink href={scopedHref(incidentsHref(), scope)}>Back to the incidents</ButtonLink>}
      >
        It may have been removed with its server, or your roles do not cover its site.
      </NotFound>
    );
  }
  if (!data || !incident || !updatedAt) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what="the incident" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading incident" />
    );
  }

  const { chart, remediation } = data;
  const now = updatedAt;
  const open = incident.state === "open";
  const format = (v: number) => formatSensorValue(incident.metric, v);
  const opened = Date.parse(incident.opened_at) / 1000;
  const resolved = incident.resolved_at ? Date.parse(incident.resolved_at) / 1000 : null;
  const tone: Tone = incident.severity === "critical" ? "bad" : "warn";
  const ratio = incident.metric.endsWith("_ratio");
  const offline = chart === null;
  const host = (
    <Link href={scopedHref(incidentPeerHref(incident), scope)} className={styles.underline}>
      {incident.hostname}
    </Link>
  );

  return (
    <Page>
      <Breadcrumb parents={[{ label: "Incidents", href: scopedHref(incidentsHref(), scope) }]} current={`#${incident.id.slice(0, 8)}`} />
      <PageHeader
        detail
        title={
          offline ? (
            <>
              {host} {open ? "is offline" : "was offline"}
            </>
          ) : (
            <>
              {title} on {host}
            </>
          )
        }
        status={
          <>
            <StatusLabel kind="severity" value={open ? incident.severity : "resolved"} />
            <span className={styles.muted}>
              {open ? "open" : `was ${SEVERITY_LABELS[incident.severity]}`} · {incident.site_name}
            </span>
          </>
        }
        meta={open ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : "Resolved: it no longer changes."}
        actions={
          <>
            {incident.server_id && !offline && (
              <ButtonLink href={scopedHref(sensorHref(incident.server_id, incident.metric, incident.instance), scope)}>Open sensor</ButtonLink>
            )}
            <ButtonLink href={scopedHref(incidentPeerHref(incident), scope)}>Open {incident.hostname}</ButtonLink>
          </>
        }
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      <StatTiles
        compact
        label="Summary"
        tiles={[
          ...(offline
            ? [{ label: "Last heard from", value: minutes(incident.opened_at), tone: open ? tone : ("plain" as const) }]
            : [
                { label: "Last value", value: format(incident.last_value), tone: open ? tone : ("plain" as const) },
                { label: "Peak", value: format(incident.peak_value) },
              ]),
          {
            label: new Date(incident.opened_at).toDateString() === now.toDateString()
              ? "Opened"
              : `Opened, ${formatShortDate(new Date(incident.opened_at))}`,
            value: minutes(incident.opened_at),
          },
          { label: open ? "Duration" : "Lasted", value: formatDuration(((resolved ?? now.getTime() / 1000) - opened)) },
        ]}
      />
      {chart === null ? null : "error" in chart ? (
        <Banner tone="bad">{chart.error}</Banner>
      ) : (
        <TimeChart
          title="Around the incident"
          subtitle={`${minutes(new Date(chart.start * 1000).toISOString())} → ${minutes(new Date(chart.end * 1000).toISOString())} · hover or use ← → for exact values`}
          actions={
            <div role="group" aria-label="Window" className={styles.segments}>
              {INCIDENT_MARGINS.map((m) => (
                <Button
                  key={m.label}
                  aria-pressed={m === margin}
                  onClick={() => setParams({ window: m === INCIDENT_MARGINS[0] ? null : m.label.replace(/[± ]/g, "") })}
                >
                  {m.label}
                </Button>
              ))}
            </div>
          }
          series={chart.series.map((s) => ({ label: s.labels.instance ? seriesTitle(undefined, incident.metric, s.labels.instance) : title, points: s.points }))}
          start={chart.start}
          end={chart.end}
          step={chart.step}
          format={format}
          min={ratio ? 0 : undefined}
          max={ratio ? 1 : undefined}
          thresholds={snapshotLines(incident.threshold)}
          markers={[
            { t: opened, label: `Opened ${minutes(incident.opened_at)}`, kind: "opened" },
            resolved !== null
              ? { t: resolved, label: `Resolved ${minutes(incident.resolved_at!)}`, kind: "resolved" }
              : { t: chart.end, label: "Now · open", kind: "now" },
          ]}
          band={{ start: opened, end: resolved ?? undefined, severity: incident.severity }}
          emptyText="No data around this incident. Metrics are kept for a limited time."
        />
      )}
      <div className={styles.pair}>
        {offline ? (
          <FactsBox
            title="Rule"
            facts={[
              { label: "Opens", value: `after ${formatSeconds(OFFLINE_AFTER_S)} without a heartbeat` },
              { label: "Resolves when", value: "it connects again, or is muted or removed" },
              { label: "Mute", value: <>Offline alerts on {host}, or for its site in Administration › Sites</> },
            ]}
          />
        ) : (
          <FactsBox title="Threshold when it opened" facts={thresholdFacts(incident.threshold, format)} />
        )}
        <TableBox title="Timeline">
          <Timeline events={incident.events} format={format} now={now} />
        </TableBox>
      </div>
      {remediation && <Remediation remediation={remediation} />}
    </Page>
  );
}

/** Warning and critical lines of the threshold the incident opened under. */
export function snapshotLines(threshold: ThresholdSnapshot): Threshold[] {
  return (["warning", "critical"] as const).flatMap((severity) => {
    const value = threshold[severity];
    return value === null ? [] : [{ severity, value, label: SEVERITY_LABELS[severity] }];
  });
}

function thresholdFacts(threshold: ThresholdSnapshot, format: (v: number) => string) {
  const above = threshold.comparison === ">" || threshold.comparison === ">=";
  const resolves = resolvesWhen(threshold, format);
  return [
    { label: "Comparison", value: threshold.comparison, mono: true },
    { label: "Warning", value: threshold.warning === null ? "none" : format(threshold.warning) },
    { label: "Critical", value: threshold.critical === null ? "none" : format(threshold.critical) },
    {
      label: "Duration",
      value: (
        <>
          {formatSeconds(threshold.duration_s)} <span className={styles.muted}>past the threshold before opening</span>
        </>
      ),
    },
    {
      label: "Hysteresis",
      value: (
        <>
          {format(threshold.hysteresis)} <span className={styles.muted}>{above ? "below" : "above"} the threshold before resolving</span>
        </>
      ),
    },
    ...(resolves ? [{ label: "Resolves when", value: resolves }] : []),
    ...(threshold.instance === null ? [] : [{ label: "Applies to", value: threshold.instance || "no instance", mono: true }]),
  ];
}

const EVENT_TONES: Record<IncidentEvent["kind"], (e: IncidentEvent) => Tone> = {
  opened: (e) => (e.severity === "critical" ? "bad" : "warn"),
  escalated: (e) => (e.severity === "critical" ? "bad" : "warn"),
  deescalated: (e) => (e.severity === "critical" ? "bad" : "warn"),
  resolved: () => "neutral",
};

/** "Opened as Warning", "Escalated to Critical", "Resolved". */
function eventWords(event: IncidentEvent): string {
  const severity = event.severity ? SEVERITY_LABELS[event.severity] : "";
  switch (event.kind) {
    case "opened":
      return `Opened as ${severity}`;
    case "escalated":
      return `Escalated to ${severity}`;
    case "deescalated":
      return `De-escalated to ${severity}`;
    case "resolved":
      return "Resolved";
  }
}

/** The events in the API's order, which is the order the engine made them. */
function Timeline({ events, format, now }: { events: IncidentEvent[]; format: (v: number) => string; now: Date }) {
  if (events.length === 0) return <p>No events recorded.</p>;
  const columns: Column<IncidentEvent>[] = [
    {
      key: "time",
      header: "Time",
      className: styles.muted,
      cell: (e) => {
        const at = new Date(e.occurred_at);
        const today = at.toDateString() === now.toDateString();
        return (
          <time dateTime={e.occurred_at} title={formatExact(at)}>
            {today ? formatClock(at) : `${formatShortDate(at)} ${formatClock(at)}`}
          </time>
        );
      },
    },
    {
      key: "event",
      header: "Event",
      cell: (e) => {
        const reason = typeof e.details.reason === "string" ? e.details.reason.replaceAll("_", " ") : null;
        return (
          <>
            <Label tone={EVENT_TONES[e.kind](e)}>{eventWords(e)}</Label>
            {reason && <span className={styles.muted}> · {reason}</span>}
          </>
        );
      },
    },
    { key: "value", header: "Value", cell: (e) => (e.value === null ? <span className={styles.muted}>—</span> : format(e.value)) },
  ];
  return <DataTable bare dense label="Timeline" columns={columns} rows={events} rowKey={(e) => String(e.id)} pageSize={100} />;
}

/** One line on what the AI remediation loop did; its decisions are made elsewhere. */
function Remediation({ remediation }: { remediation: RemediationIncident }) {
  return (
    <div className={styles.remediation}>
      <strong>AI remediation</strong>
      <StatusLabel kind="remediation" value={remediation.state} />
      <span>
        {remediation.event_count} {remediation.event_count === 1 ? "event" : "events"} · opened {minutes(remediation.opened_at)}
        {remediation.closed_at && ` · closed ${minutes(remediation.closed_at)}`}
      </span>
    </div>
  );
}
