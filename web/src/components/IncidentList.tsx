"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useState } from "react";

import { type Incident, type Server, type Site, api, describeError, isUnauthorized } from "@/lib/api";
import { type IncidentFilters, apiQuery, filtersFromQuery } from "@/lib/incidents";
import { incidentHref, serverHref } from "@/lib/routes";
import { type Scope, scopedHref, useScope } from "@/lib/scope";
import { formatSensorValue, seriesTitle } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { replaceParams } from "@/lib/urlState";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar } from "@/components/ui/FilterBar";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { formatDay, formatDuration, formatExact, formatSince } from "@/components/ui/time";

import styles from "./IncidentList.module.css";

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

  const inScope = scopeSites(scope, sites);
  const visible = (i: Incident) => !inScope || inScope.has(i.site_id);
  const incidents = data?.incidents.filter(visible) ?? [];
  const open = data?.open.filter(visible) ?? [];
  const customers = new Map(sites.map((s) => [s.id, s.customer_name]));
  const now = updatedAt ?? new Date();
  const filtered = Boolean(filters.severity || filters.serverId || filters.state !== "all" || (filters.siteId && !scope.siteId));

  const siteChoices = sites.filter((s) => !scope.customerId || s.customer_id === scope.customerId);
  const serverChoices = servers
    .filter((s) => (!scope.customerId || s.customer_id === scope.customerId) && (!filters.siteId || s.site_id === filters.siteId))
    .sort((a, b) => a.hostname.localeCompare(b.hostname, "en", { numeric: true }));

  const columns: Column<Incident>[] = [
    {
      key: "severity",
      header: "Severity",
      cell: (i) =>
        i.state === "open" ? (
          <StatusLabel kind="severity" value={i.severity} />
        ) : (
          <>
            <StatusLabel kind="severity" value="resolved" />{" "}
            <span className={styles.small}>was {i.severity === "critical" ? "Critical" : "Warning"}</span>
          </>
        ),
    },
    {
      key: "server",
      header: "Server",
      cell: (i) => (
        <PrimaryCell
          title={i.hostname}
          href={scopedHref(serverHref(i.server_id), scope)}
          sub={[i.site_name, customers.get(i.site_id)].filter(Boolean).join(" · ")}
        />
      ),
    },
    {
      key: "metric",
      header: "Metric",
      cell: (i) => (
        <Link href={scopedHref(incidentHref(i.id), scope)} className={i.state === "open" ? styles.strong : undefined}>
          {seriesTitle(undefined, i.metric, i.instance)}
        </Link>
      ),
    },
    {
      key: "opened",
      header: "Opened",
      cell: (i) => (
        <time dateTime={i.opened_at} title={formatExact(new Date(i.opened_at))}>
          {formatDay(new Date(i.opened_at), now)}
        </time>
      ),
    },
    { key: "state", header: "State", cell: (i) => (i.resolved_at ? `Resolved ${formatSince(new Date(i.resolved_at), now)}` : "Open") },
    {
      key: "duration",
      header: "Duration",
      cell: (i) => formatDuration(((i.resolved_at ? Date.parse(i.resolved_at) : now.getTime()) - Date.parse(i.opened_at)) / 1000),
    },
    {
      key: "last",
      header: "Last value",
      cell: (i) => <span className={i.state === "open" ? styles.strong : undefined}>{formatSensorValue(i.metric, i.last_value)}</span>,
    },
    { key: "peak", header: "Peak", cell: (i) => formatSensorValue(i.metric, i.peak_value) },
  ];

  const critical = open.filter((i) => i.severity === "critical").length;
  return (
    <Page>
      <PageHeader
        title="Incidents"
        meta={
          <>
            {data && `${open.length} open, ${critical} critical · `}
            <RefreshLine updatedAt={updatedAt} stale={stale} />
          </>
        }
      />
      <FilterBar
        filters={[
          {
            name: "state",
            label: "State",
            options: [
              { value: "open", label: "Open" },
              { value: "resolved", label: "Resolved" },
            ],
          },
          {
            name: "severity",
            label: "Severity",
            options: [
              { value: "critical", label: "Critical" },
              { value: "warning", label: "Warning" },
            ],
          },
          ...(scope.siteId ? [] : [{ name: "site", label: "Site", options: siteChoices.map((s) => ({ value: s.id, label: s.name })) }]),
          { name: "server", label: "Server", options: serverChoices.map((s) => ({ value: s.id, label: s.hostname })) },
        ]}
        // Another site's server would match nothing.
        onChange={(next) => next.get("site") !== (filters.siteId ?? null) && next.get("server") && replaceParams({ server: null })}
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what="incidents" error={describeError(error)} onRetry={refresh} />
      ) : data && incidents.length === 0 ? (
        <EmptyState>{filtered ? "No incidents match these filters." : "No incidents. Thresholds have not fired yet."}</EmptyState>
      ) : (
        <DataTable
          label="Incidents"
          columns={columns}
          rows={incidents}
          rowKey={(i) => i.id}
          rowMuted={(i) => i.state === "resolved"}
          loading={!data}
          hasMore={Boolean(data?.more) && limit < MAX_LIMIT}
          onLoadMore={() => setLimit((n) => Math.min(MAX_LIMIT, n + PAGE_SIZE))}
          summary={(shown) => `Showing ${shown} ${shown === 1 ? "incident" : "incidents"} · ${PAGE_SIZE} per page`}
        />
      )}
    </Page>
  );
}
