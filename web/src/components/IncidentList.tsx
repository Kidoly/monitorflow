"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useState } from "react";

import { t } from "@/i18n";
import { type Incident, type Server, type Site, api, describeError, isUnauthorized } from "@/lib/api";
import { type IncidentFilters, apiQuery, filtersFromQuery, incidentTitle, isOffline } from "@/lib/incidents";
import { administersEverything, multiCustomer, useMe } from "@/lib/me";
import { NOTIFICATIONS_PATH, incidentHref, incidentPeerHref, incidentsHref } from "@/lib/routes";
import { type Scope, scopedHref, useScope } from "@/lib/scope";
import { formatSensorValue } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { replaceParams } from "@/lib/urlState";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { ButtonLink } from "@/components/ui/Button";
import { FilterBar, NoMatches } from "@/components/ui/FilterBar";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatDay, formatDuration, formatExact, formatSince } from "@/components/ui/time";

import { AcknowledgeButton } from "./AcknowledgeButton";
import styles from "./IncidentList.module.css";

/** A threshold incident's value; offline ones have none. */
const value = (i: Incident, v: number) => (isOffline(i) ? "" : formatSensorValue(i.metric, v));

/** Whether a silence keeps this incident quiet now, or did. */
function SilencedSign({ incident, now }: { incident: Incident; now: Date }) {
  if (incident.silenced_until) {
    return (
      <>
        {" "}
        <Label tone="neutral">{t("incidents.silenced")}</Label>{" "}
        <span className={styles.small}>{t("incidents.silencedUntil", { when: formatDay(new Date(incident.silenced_until), now) })}</span>
      </>
    );
  }
  return incident.silenced ? <span className={styles.small}>{t("incidents.wasSilencedShort")}</span> : null;
}

/** Who said they are on it (ADR-0082), or the button to say so. */
function Acknowledged({
  incident,
  now,
  onAcknowledge,
}: {
  incident: Incident;
  now: Date;
  onAcknowledge: (ids: string[], note: string) => Promise<void>;
}) {
  if (incident.state !== "open") return null;
  if (incident.acknowledged_at) {
    return (
      <span className={styles.small}>
        {t("incidents.acknowledgedBy", {
          name: incident.acknowledged_by ?? t("incidents.someone"),
          when: formatSince(new Date(incident.acknowledged_at), now),
        })}
        {incident.acknowledged_note && `: ${incident.acknowledged_note}`}
      </span>
    );
  }
  if (!incident.can_acknowledge) return null;
  return (
    <>
      {" "}
      <AcknowledgeButton
        size="small"
        name={t("incidents.acknowledgeHost", { host: incident.hostname })}
        onAcknowledge={(note) => onAcknowledge([incident.id], note)}
        describeError={describeError}
      />
    </>
  );
}

/** Incidents per "Load more". */
export const PAGE_SIZE = 50;
/** The API's largest page, which caps how far "Load more" goes. */
const MAX_LIMIT = 500;

/** The incidents list for the page's query string, which holds its filters. */
export function IncidentsView() {
  const search = useSearchParams().toString();
  const filters = useMemo(() => filtersFromQuery(new URLSearchParams(search)), [search]);
  // New filters start from the first page instead of extending the old list.
  return <IncidentList key={search} filters={filters} />;
}

interface Loaded {
  incidents: Incident[];
  more: boolean;
  /** Every open incident, for the counts in the title line. */
  open: Incident[];
}

/** Sites of the scope's customer; null is every site. */
function scopeSites(scope: Scope, sites: Site[]): Set<string> | null {
  if (scope.siteId) return new Set([scope.siteId]);
  if (!scope.customerId) return null;
  return new Set(sites.filter((s) => s.customer_id === scope.customerId).map((s) => s.id));
}

export function IncidentList({ filters }: { filters: IncidentFilters }) {
  const scope = useScope();
  const [limit, setLimit] = useState(PAGE_SIZE);
  const [sites, setSites] = useState<Site[]>([]);
  const [servers, setServers] = useState<Server[]>([]);

  // Filter choices and customer names load once; a failure leaves the choices in the URL.
  useEffect(() => {
    const controller = new AbortController();
    api.sites(controller.signal).then((r) => setSites(r.sites), () => {});
    api.servers(controller.signal).then((r) => setServers(r.servers), () => {});
    return () => controller.abort();
  }, []);

  const fetcher = useCallback(
    async (signal: AbortSignal): Promise<Loaded> => {
      const [page, open] = await Promise.all([
        api.incidents({ ...apiQuery(filters), limit }, signal),
        api.incidents({ state: "open", limit: MAX_LIMIT }, signal),
      ]);
      return { incidents: page.incidents, more: page.next_cursor !== null, open: open.incidents };
    },
    [filters, limit],
  );
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  async function acknowledge(ids: string[], note: string) {
    await api.acknowledgeIncidents(ids, note);
    refresh();
  }

  const me = useMe();
  const inScope = scopeSites(scope, sites);
  const visible = (i: Incident) => !inScope || inScope.has(i.site_id);
  const incidents = data?.incidents.filter(visible) ?? [];
  const open = data?.open.filter(visible) ?? [];
  const customers = new Map(sites.map((s) => [s.id, s.customer_name]));
  const now = updatedAt ?? new Date();
  const filtered = Boolean(
    filters.severity || filters.serverId || filters.groupId || filters.state !== "all" || (filters.siteId && !scope.siteId),
  );

  const siteChoices = sites.filter((s) => !scope.customerId || s.customer_id === scope.customerId);
  const serverChoices = servers
    .filter((s) => (!scope.customerId || s.customer_id === scope.customerId) && (!filters.siteId || s.site_id === filters.siteId))
    .sort((a, b) => a.hostname.localeCompare(b.hostname, "en", { numeric: true }));

  const columns: Column<Incident>[] = [
    {
      key: "severity",
      header: t("incidents.column.severity"),
      cell: (i) =>
        i.state === "open" ? (
          <StatusLabel kind="severity" value={i.severity} />
        ) : (
          <>
            <StatusLabel kind="severity" value="resolved" />{" "}
            <span className={styles.small}>{t("incidents.was", { severity: t(`sensors.severity.${i.severity}`) })}</span>
          </>
        ),
    },
    {
      key: "server",
      header: t("incidents.column.server"),
      cell: (i) => (
        <PrimaryCell
          title={i.hostname}
          href={scopedHref(incidentPeerHref(i), scope)}
          sub={[i.site_name, multiCustomer(me) && customers.get(i.site_id)].filter(Boolean).join(" · ")}
        />
      ),
    },
    {
      key: "metric",
      header: t("incidents.column.metric"),
      cell: (i) => (
        <Link href={scopedHref(incidentHref(i.id), scope)} className={i.state === "open" ? styles.strong : undefined}>
          {incidentTitle(i)}
        </Link>
      ),
    },
    {
      key: "opened",
      header: t("incidents.column.opened"),
      cell: (i) => (
        <time dateTime={i.opened_at} title={formatExact(new Date(i.opened_at))}>
          {formatDay(new Date(i.opened_at), now)}
        </time>
      ),
    },
    {
      key: "state",
      header: t("incidents.column.state"),
      cell: (i) => (
        <>
          {i.resolved_at ? t("incidents.resolvedSince", { when: formatSince(new Date(i.resolved_at), now) }) : t("incidents.open")}
          <SilencedSign incident={i} now={now} />
          <Acknowledged incident={i} now={now} onAcknowledge={acknowledge} />
        </>
      ),
    },
    {
      key: "duration",
      header: t("incidents.column.duration"),
      cell: (i) => formatDuration(((i.resolved_at ? Date.parse(i.resolved_at) : now.getTime()) - Date.parse(i.opened_at)) / 1000),
    },
    {
      key: "last",
      header: t("incidents.column.last"),
      cell: (i) => <span className={i.state === "open" ? styles.strong : undefined}>{value(i, i.last_value)}</span>,
    },
    { key: "peak", header: t("incidents.column.peak"), cell: (i) => value(i, i.peak_value) },
  ];

  const critical = open.filter((i) => i.severity === "critical").length;
  const acknowledgeable = incidents.filter((i) => i.state === "open" && i.can_acknowledge && !i.acknowledged_at);
  return (
    <Page>
      <PageHeader
        title={t("incidents.title")}
        meta={
          <>
            {data && `${t("incidents.count.open", { count: open.length })}, ${t("incidents.count.critical", { count: critical })} · `}
            <RefreshLine updatedAt={updatedAt} stale={stale} />
          </>
        }
      />
      <FilterBar
        filters={[
          {
            name: "state",
            label: t("incidents.filter.state"),
            options: [
              { value: "open", label: t("incidents.open") },
              { value: "resolved", label: t("incidents.resolved") },
            ],
          },
          {
            name: "severity",
            label: t("incidents.filter.severity"),
            options: [
              { value: "critical", label: t("sensors.severity.critical") },
              { value: "warning", label: t("sensors.severity.warning") },
            ],
          },
          ...(scope.siteId ? [] : [{ name: "site", label: t("incidents.filter.site"), options: siteChoices.map((s) => ({ value: s.id, label: s.name })) }]),
          { name: "server", label: t("incidents.filter.server"), options: serverChoices.map((s) => ({ value: s.id, label: s.hostname })) },
        ]}
        // Another site's server would match nothing.
        onChange={(next) => next.get("site") !== (filters.siteId ?? null) && next.get("server") && replaceParams({ server: null })}
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {filters.groupId && (
        <Banner tone="info">
          {t("incidents.group.banner")}{" "}
          <Link href={scopedHref(incidentsHref({ ...filters, groupId: null }), scope)}>{t("incidents.group.showAll")}</Link>
          {acknowledgeable.length > 1 && (
            <>
              {" "}
              <AcknowledgeButton
                size="small"
                variant="k"
                label={t("incidents.group.acknowledgeAll", { count: acknowledgeable.length })}
                onAcknowledge={(note) => acknowledge(acknowledgeable.map((i) => i.id), note)}
                describeError={describeError}
              />
            </>
          )}
        </Banner>
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("incidents.load.list")} error={describeError(error)} onRetry={refresh} />
      ) : data && incidents.length === 0 ? (
        filtered ? (
          <NoMatches names={["state", "severity", "server", "group", ...(scope.siteId ? [] : ["site"])]}>
            {t("incidents.noMatch")}
          </NoMatches>
        ) : (
          <EmptyState
            actions={
              administersEverything(me) ? (
                <ButtonLink href={NOTIFICATIONS_PATH}>{t("incidents.setUpNotifications")}</ButtonLink>
              ) : (
                <ButtonLink href={scopedHref("/", scope)}>{t("incidents.backToFleet")}</ButtonLink>
              )
            }
          >
            {t("incidents.empty")}
          </EmptyState>
        )
      ) : (
        <DataTable
          label={t("incidents.title")}
          columns={columns}
          rows={incidents}
          rowKey={(i) => i.id}
          rowMuted={(i) => i.state === "resolved"}
          loading={!data}
          hasMore={Boolean(data?.more) && limit < MAX_LIMIT}
          onLoadMore={() => setLimit((n) => Math.min(MAX_LIMIT, n + PAGE_SIZE))}
          summary={(shown) => t("incidents.summary", { count: shown, perPage: PAGE_SIZE })}
        />
      )}
    </Page>
  );
}
