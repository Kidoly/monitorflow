"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useRef } from "react";

import { rich, t } from "@/i18n";
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
import { followUpLine, listNames, minutesText } from "@/lib/followUp";
import { INCIDENT_MARGINS, OFFLINE_AFTER_S, incidentTitle, incidentWindow, isOffline, resolvesWhen } from "@/lib/incidents";
import { useMultiSite } from "@/lib/me";
import { incidentLogsHref, incidentPeerHref, incidentsHref, sensorHref } from "@/lib/routes";
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
import { formatClock, formatDay, formatDuration, formatExact, formatShortDate } from "@/components/ui/time";
import type { Tone } from "@/components/ui/tone";

import { AcknowledgeButton } from "./AcknowledgeButton";
import { IncidentDependencies } from "./Dependencies";
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
  const severalSites = useMultiSite();
  const [params, setParams] = useUrlParams();
  const margin = INCIDENT_MARGINS.find((m) => m.param === params.get("window")) ?? INCIDENT_MARGINS[0];
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
    if (incident) document.title = t("incidents.documentTitle", { title, host: incident.hostname });
  }, [incident, title]);

  if (!valid || (error instanceof ApiError && error.status === 404)) {
    return (
      <NotFound
        title={t("incidents.notFound")}
        actions={<ButtonLink href={scopedHref(incidentsHref(), scope)}>{t("incidents.backToIncidents")}</ButtonLink>}
      >
        {t("incidents.notFoundText")}
      </NotFound>
    );
  }
  if (!data || !incident || !updatedAt) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what={t("incidents.load.one")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("incidents.loading")} />
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
      <Breadcrumb parents={[{ label: t("incidents.title"), href: scopedHref(incidentsHref(), scope) }]} current={`#${incident.id.slice(0, 8)}`} />
      <PageHeader
        detail
        title={
          offline ? (
            rich(open ? "incidents.isOffline" : "incidents.wasOffline", { host })
          ) : (
            rich("incidents.on", { title, host })
          )
        }
        status={
          <>
            <StatusLabel kind="severity" value={open ? incident.severity : "resolved"} />
            <span className={styles.muted}>
              {open ? t("incidents.openWord") : t("incidents.was", { severity: SEVERITY_LABELS[incident.severity] })}
              {severalSites && ` · ${incident.site_name}`}
            </span>
            {incident.silenced_until ? (
              <>
                <Label tone="neutral">{incident.in_maintenance ? t("incidents.inMaintenance") : t("incidents.silenced")}</Label>
                <span className={styles.muted}>
                  {incident.in_maintenance
                    ? t("incidents.inMaintenanceText", { when: formatDay(new Date(incident.silenced_until), now) })
                    : t("incidents.silencedText", { when: formatDay(new Date(incident.silenced_until), now) })}
                </span>
              </>
            ) : (
              incident.silenced && <Label tone="neutral">{t("incidents.wasSilenced")}</Label>
            )}
          </>
        }
        meta={open ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : undefined}
        actions={
          <>
            {incident.server_id && !offline && (
              <ButtonLink href={scopedHref(sensorHref(incident.server_id, incident.metric, incident.instance), scope)}>{t("incidents.openSensor")}</ButtonLink>
            )}
            {incident.server_id && (
              <ButtonLink href={scopedHref(incidentLogsHref(incident.server_id, incident.opened_at, incident.resolved_at), scope)}>
                {t("incidents.logsAround")}
              </ButtonLink>
            )}
          </>
        }
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      <Acknowledgement incident={incident} now={now} onDone={refresh} />
      <SentTogether incident={incident} now={now} />
      <IncidentDependencies incident={incident} />
      <StatTiles
        compact
        label={t("incidents.tile.summary")}
        tiles={[
          ...(offline
            ? [{ label: t("incidents.tile.lastHeard"), value: minutes(incident.opened_at), tone: open ? tone : ("plain" as const) }]
            : [
                { label: t("incidents.tile.last"), value: format(incident.last_value), tone: open ? tone : ("plain" as const) },
                { label: t("incidents.tile.peak"), value: format(incident.peak_value) },
              ]),
          {
            label: new Date(incident.opened_at).toDateString() === now.toDateString()
              ? t("incidents.tile.opened")
              : t("incidents.tile.openedOn", { day: formatShortDate(new Date(incident.opened_at)) }),
            value: minutes(incident.opened_at),
          },
          { label: open ? t("incidents.tile.duration") : t("incidents.tile.lasted"), value: formatDuration(((resolved ?? now.getTime() / 1000) - opened)) },
        ]}
      />
      {chart === null ? null : "error" in chart ? (
        <Banner tone="bad">{chart.error}</Banner>
      ) : (
        <TimeChart
          title={t("incidents.chart.title")}
          subtitle={t("incidents.chart.subtitle", {
            from: minutes(new Date(chart.start * 1000).toISOString()),
            to: minutes(new Date(chart.end * 1000).toISOString()),
          })}
          actions={
            <div role="group" aria-label={t("incidents.chart.window")} className={styles.segments}>
              {INCIDENT_MARGINS.map((m) => (
                <Button
                  key={m.param}
                  aria-pressed={m === margin}
                  onClick={() => setParams({ window: m === INCIDENT_MARGINS[0] ? null : m.param })}
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
            { t: opened, label: t("incidents.chart.opened", { time: minutes(incident.opened_at) }), kind: "opened" },
            resolved !== null
              ? { t: resolved, label: t("incidents.chart.resolved", { time: minutes(incident.resolved_at!) }), kind: "resolved" }
              : { t: chart.end, label: t("incidents.chart.now"), kind: "now" },
          ]}
          band={{ start: opened, end: resolved ?? undefined, severity: incident.severity }}
          emptyText={t("incidents.chart.empty")}
        />
      )}
      <div className={styles.pair}>
        {offline ? (
          <FactsBox
            title={t("incidents.rule.title")}
            facts={[
              { label: t("incidents.rule.opens"), value: t("incidents.rule.opensValue", { duration: formatSeconds(OFFLINE_AFTER_S) }) },
              { label: t("incidents.rule.resolvesWhen"), value: t("incidents.rule.resolvesValue") },
              { label: t("incidents.rule.turnOff"), value: rich("incidents.rule.turnOffValue", { host }) },
            ]}
          />
        ) : (
          <FactsBox title={t("incidents.threshold.title")} facts={thresholdFacts(incident.threshold, format)} />
        )}
        {/* The opening alone is already in the tiles, the chart and the header. */}
        {!onlyOpened(incident.events) && (
          <TableBox title={t("incidents.timeline")}>
            <Timeline events={incident.events} format={format} now={now} />
          </TableBox>
        )}
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
    { label: t("incidents.threshold.comparison"), value: threshold.comparison, mono: true },
    {
      label: t("incidents.threshold.warning"),
      value: threshold.warning === null ? t("incidents.threshold.none") : format(threshold.warning),
    },
    {
      label: t("incidents.threshold.critical"),
      value: threshold.critical === null ? t("incidents.threshold.none") : format(threshold.critical),
    },
    {
      label: t("incidents.threshold.duration"),
      value: (
        <>
          {formatSeconds(threshold.duration_s)} <span className={styles.muted}>{t("incidents.threshold.durationNote")}</span>
        </>
      ),
    },
    // With a value to resolve at, the margin is in it, worked out.
    resolves
      ? { label: t("incidents.rule.resolvesWhen"), value: resolves }
      : {
          label: t("incidents.threshold.margin"),
          value: (
            <>
              {format(threshold.hysteresis)}{" "}
              <span className={styles.muted}>{above ? t("incidents.threshold.marginBelow") : t("incidents.threshold.marginAbove")}</span>
            </>
          ),
        },
    ...(threshold.instance === null
      ? []
      : [{ label: t("incidents.threshold.appliesTo"), value: threshold.instance || t("incidents.threshold.noInstance"), mono: true }]),
  ];
}

const EVENT_TONES: Record<IncidentEvent["kind"], (e: IncidentEvent) => Tone> = {
  opened: (e) => (e.severity === "critical" ? "bad" : "warn"),
  escalated: (e) => (e.severity === "critical" ? "bad" : "warn"),
  deescalated: (e) => (e.severity === "critical" ? "bad" : "warn"),
  resolved: () => "neutral",
  silenced: () => "neutral",
  silence_ended: () => "neutral",
  acknowledged: () => "ok",
  followed_up: () => "warn",
};

/** "Opened as Warning", "Escalated to Critical", "Resolved". */
function eventWords(event: IncidentEvent): string {
  const severity = event.severity ? SEVERITY_LABELS[event.severity] : "";
  switch (event.kind) {
    case "opened":
      return t("incidents.event.opened", { severity });
    case "escalated":
      return t("incidents.event.escalated", { severity });
    case "deescalated":
      return t("incidents.event.deescalated", { severity });
    case "resolved":
      return t("incidents.event.resolved");
    case "silenced":
      return t("incidents.event.silenced");
    case "silence_ended":
      return event.details.notified ? t("incidents.event.silenceSent") : t("incidents.event.silenceQuiet");
    case "acknowledged":
      return `${
        typeof event.details.username === "string"
          ? t("incidents.event.acknowledgedBy", { name: event.details.username })
          : t("incidents.event.acknowledged")
      }${typeof event.details.note === "string" ? `: ${event.details.note}` : ""}`;
    case "followed_up": {
      const names = Array.isArray(event.details.told) ? event.details.told.filter((n) => typeof n === "string") : [];
      const told = names.length ? t("incidents.event.told", { names: listNames(names) }) : t("incidents.event.nobody");
      return typeof event.details.minutes === "number"
        ? t("incidents.event.notAcknowledgedAfter", { minutes: minutesText(event.details.minutes), told })
        : t("incidents.event.notAcknowledged", { told });
    }
  }
}

/**
 * Who said they are on it, or the button to say so, and who is told next
 * if nobody does (ADR-0082).
 */
function Acknowledgement({ incident, now, onDone }: { incident: IncidentDetail; now: Date; onDone: () => void }) {
  const followUp = followUpLine(incident, incident.follow_up, (iso) => formatDay(new Date(iso), now));
  if (incident.acknowledged_at) {
    return (
      <Banner tone="plain">
        {rich("incidents.ack.banner", {
          name: <strong>{incident.acknowledged_by ?? t("incidents.someone")}</strong>,
          when: formatDay(new Date(incident.acknowledged_at), now),
        })}
        {incident.acknowledged_via === "link" && t("incidents.ack.fromLink")}
        {incident.acknowledged_note ? `: ${incident.acknowledged_note}` : "."}
      </Banner>
    );
  }
  if (!incident.can_acknowledge && !followUp) return null;

  async function acknowledge(note: string) {
    await api.acknowledgeIncidents([incident.id], note);
    onDone();
  }

  return (
    <Banner tone="plain">
      <div className={styles.acknowledge}>
        {incident.can_acknowledge && (
          <>
            <AcknowledgeButton variant="k" onAcknowledge={acknowledge} describeError={describeError} />
            {/* One line: the follow-up line, when there is one, says what acknowledging stops. */}
            {!followUp && <span className={styles.muted}>{t("incidents.ack.says")}</span>}
          </>
        )}
      </div>
      {followUp && <p className={styles.followUp}>{followUp}</p>}
    </Banner>
  );
}

/** Whether the incident has done nothing yet but open. */
export function onlyOpened(events: IncidentEvent[]): boolean {
  return events.length === 1 && events[0]!.kind === "opened";
}

/** The last notification that told this incident with others (ADR-0080), and the way to the others. */
function SentTogether({ incident, now }: { incident: IncidentDetail; now: Date }) {
  const scope = useScope();
  const group = incident.notification_groups?.at(-1);
  if (!group) return null;
  const others = group.incidents - 1;
  return (
    <Banner tone="plain">
      {t("incidents.sentTogether", { count: others, when: formatDay(new Date(group.created_at), now) })}{" "}
      <Link href={scopedHref(incidentsHref({ groupId: group.id }), scope)}>{t("incidents.showThem")}</Link>
    </Banner>
  );
}

/** The events in the API's order, which is the order the engine made them. */
function Timeline({ events, format, now }: { events: IncidentEvent[]; format: (v: number) => string; now: Date }) {
  if (events.length === 0) return <p>{t("incidents.timeline.empty")}</p>;
  const columns: Column<IncidentEvent>[] = [
    {
      key: "time",
      header: t("incidents.timeline.time"),
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
      header: t("incidents.timeline.event"),
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
    { key: "value", header: t("incidents.timeline.value"), cell: (e) => (e.value === null ? null : format(e.value)) },
  ];
  return <DataTable bare dense label={t("incidents.timeline")} columns={columns} rows={events} rowKey={(e) => String(e.id)} pageSize={100} />;
}

/** One line on what the AI remediation loop did; its decisions are made elsewhere. */
function Remediation({ remediation }: { remediation: RemediationIncident }) {
  return (
    <div className={styles.remediation}>
      <strong>{t("incidents.ai.title")}</strong>
      <StatusLabel kind="remediation" value={remediation.state} />
      <span>
        {t("incidents.ai.events", { count: remediation.event_count, time: minutes(remediation.opened_at) })}
        {remediation.closed_at && t("incidents.ai.closed", { time: minutes(remediation.closed_at) })}
      </span>
    </div>
  );
}
