"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type ReactNode, useCallback, useEffect, useState } from "react";

import {
  ApiError,
  type IncidentDetail,
  type IncidentEvent,
  type MetricSeries,
  type ThresholdSnapshot,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { type Unit, formatDateTime, formatSeconds, formatValue, unitOf } from "@/lib/format";
import { incidentSeconds, incidentWindow } from "@/lib/incidents";
import { metricTitle } from "@/lib/metrics";
import { incidentsHref, serverHref } from "@/lib/routes";
import { SEVERITY_LABELS } from "@/lib/thresholds";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";

import { REFRESH_MS } from "./FleetOverview";
import { SeverityBadge } from "./IncidentBadges";
import { Time } from "./IncidentList";
import styles from "./IncidentPage.module.css";
import { metricsError } from "./ServerGraphs";
import { type ReferenceLine, TimeSeriesChart } from "./TimeSeriesChart";

/** The incident page for the `id` query parameter. */
export function IncidentView() {
  const id = useSearchParams().get("id");
  return <IncidentPage key={id} id={id} />;
}

type Chart = { start: number; end: number; step: number } & ({ series: MetricSeries[] } | { error: string });

interface State {
  /** Undefined until loaded, null when not found. */
  incident: IncidentDetail | null | undefined;
  chart: Chart | null;
  updatedAt: Date | null;
  error: string | null;
}

export function IncidentPage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const [state, setState] = useState<State>({ incident: undefined, chart: null, updatedAt: null, error: null });

  const load = useCallback(
    async (signal: AbortSignal) => {
      try {
        const incident = await api.incident(id ?? "", signal);
        const now = new Date();
        const window = incidentWindow(incident.opened_at, incident.resolved_at, now);
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
        setState({ incident, chart, updatedAt: now, error: null });
      } catch (e) {
        if (isAbortError(e)) return;
        if (e instanceof ApiError && e.status === 404) {
          setState((s) => ({ ...s, incident: null }));
        } else if (!(e instanceof ApiError && e.status === 401)) {
          // A 401 has already been routed to the login page.
          setState((s) => ({ ...s, error: describeError(e) }));
        }
      }
    },
    [id],
  );
  // A resolved incident does not change any more.
  usePolling(load, REFRESH_MS, valid && state.incident?.state !== "resolved");

  const { incident, chart, updatedAt, error } = state;
  useEffect(() => {
    if (incident) document.title = `${metricTitle(incident.metric)} on ${incident.hostname} · MonitorFlow`;
  }, [incident]);

  if (!valid || incident === null) {
    return (
      <section className={styles.notFound}>
        <h1>Incident not found</h1>
        <p>It may have been removed with its server, or your roles do not cover its site.</p>
        <p>
          <Link href={incidentsHref()}>Back to the incidents</Link>
        </p>
      </section>
    );
  }
  if (!incident || !updatedAt) {
    return error ? (
      <p className={styles.error} role="alert">
        {error}
      </p>
    ) : (
      <p role="status">Loading incident…</p>
    );
  }

  const unit = unitOf(incident.metric);
  const title = metricTitle(incident.metric);
  return (
    <article aria-labelledby="incident-title">
      <header className={styles.header}>
        <p className={styles.crumbs}>
          <Link href={incidentsHref()}>Incidents</Link> / {incident.site_name}
        </p>
        <div className={styles.title}>
          <h1 id="incident-title">
            {title}
            {incident.instance && <code className={styles.instance}>{incident.instance}</code>} on{" "}
            <Link href={serverHref(incident.server_id)}>{incident.hostname}</Link>
          </h1>
          <SeverityBadge severity={incident.severity} state={incident.state} />
          <span>{incident.state === "open" ? "Open" : "Resolved"}</span>
        </div>
        <dl className={styles.facts}>
          <Fact term="Opened">{formatDateTime(incident.opened_at)}</Fact>
          {incident.resolved_at && <Fact term="Resolved">{formatDateTime(incident.resolved_at)}</Fact>}
          <Fact term={incident.resolved_at ? "Lasted" : "Open for"}>
            {formatSeconds(incidentSeconds(incident.opened_at, incident.resolved_at, updatedAt))}
          </Fact>
          <Fact term="Last value">
            {formatValue(incident.last_value, unit)}, <Time iso={incident.last_value_at} now={updatedAt} />
          </Fact>
          <Fact term="Peak">{formatValue(incident.peak_value, unit)}</Fact>
        </dl>
        <ThresholdFacts threshold={incident.threshold} unit={unit} />
      </header>
      {error && (
        <p className={styles.error} role="alert">
          Refresh failed: {error} Showing the last data received.
        </p>
      )}
      <section className={styles.section} aria-labelledby="chart-title">
        <h2 id="chart-title">Around the incident</h2>
        {chart && "error" in chart ? (
          <p className={styles.error} role="alert">
            {chart.error}
          </p>
        ) : (
          chart && (
            <TimeSeriesChart
              title={title}
              unit={unit}
              start={chart.start}
              end={chart.end}
              step={chart.step}
              series={chart.series.map((s) => ({ label: s.labels.instance || title, points: s.points }))}
              referenceLines={snapshotLines(incident.threshold)}
              markers={[
                { t: Date.parse(incident.opened_at) / 1000, label: "Opened" },
                ...(incident.resolved_at ? [{ t: Date.parse(incident.resolved_at) / 1000, label: "Resolved" }] : []),
              ]}
              emptyText="No data around this incident. Metrics are kept for a limited time."
            />
          )
        )}
      </section>
      <section className={styles.section} aria-labelledby="timeline-title">
        <h2 id="timeline-title">Timeline</h2>
        <Timeline events={incident.events} unit={unit} />
      </section>
    </article>
  );
}

const COMPARISONS: Record<string, string> = {
  ">": "above",
  ">=": "at or above",
  "<": "below",
  "<=": "at or below",
};

/** Warning and critical lines of the threshold the incident opened under. */
export function snapshotLines(threshold: ThresholdSnapshot): ReferenceLine[] {
  return (["warning", "critical"] as const).flatMap((severity) => {
    const value = threshold[severity];
    return value === null ? [] : [{ severity, value, label: SEVERITY_LABELS[severity] }];
  });
}

function ThresholdFacts({ threshold, unit }: { threshold: ThresholdSnapshot; unit: Unit }) {
  return (
    <section className={styles.threshold} aria-labelledby="threshold-title">
      <h2 id="threshold-title">Threshold when it opened</h2>
      <dl className={styles.facts}>
        <Fact term="Fires when">
          {COMPARISONS[threshold.comparison] ?? threshold.comparison} the threshold (
          <code>{threshold.comparison}</code>)
        </Fact>
        <Fact term="Warning">{formatValue(threshold.warning, unit)}</Fact>
        <Fact term="Critical">{formatValue(threshold.critical, unit)}</Fact>
        <Fact term="For">{formatSeconds(threshold.duration_s)}</Fact>
        <Fact term="Hysteresis">{formatValue(threshold.hysteresis, unit)}</Fact>
        <Fact term="Applies to">
          {threshold.instance === null ? "every instance" : <code>{threshold.instance || "no instance"}</code>}
        </Fact>
      </dl>
    </section>
  );
}

const EVENT_LABELS: Record<string, string> = {
  opened: "Opened",
  escalated: "Escalated",
  deescalated: "De-escalated",
  resolved: "Resolved",
};

/** The events in the API's order, which is the order the engine made them. */
function Timeline({ events, unit }: { events: IncidentEvent[]; unit: Unit }) {
  if (events.length === 0) return <p className={styles.muted}>No events recorded.</p>;
  return (
    <ol className={styles.timeline}>
      {events.map((event) => {
        const reason = typeof event.details.reason === "string" ? event.details.reason.replaceAll("_", " ") : null;
        return (
          <li key={event.id}>
            <time dateTime={event.occurred_at}>{formatDateTime(event.occurred_at)}</time>
            <strong>{EVENT_LABELS[event.kind] ?? event.kind}</strong>
            {event.severity && <SeverityBadge severity={event.severity} />}
            {event.value !== null && <span>at {formatValue(event.value, unit)}</span>}
            {reason && <span className={styles.muted}>({reason})</span>}
          </li>
        );
      })}
    </ol>
  );
}

function Fact({ term, children }: { term: string; children: ReactNode }) {
  return (
    <div>
      <dt>{term}</dt>
      <dd>{children}</dd>
    </div>
  );
}
