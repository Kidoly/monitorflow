"use client";

import Link from "next/link";
import { useRouter, useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { ApiError, type Incident, type Server, type Site, api, describeError, isAbortError } from "@/lib/api";
import { formatDateTime, formatRelative, formatSeconds, formatValue, unitOf } from "@/lib/format";
import { type IncidentFilters, NO_FILTERS, apiQuery, filtersFromQuery, incidentSeconds } from "@/lib/incidents";
import { metricTitle } from "@/lib/metrics";
import { incidentHref, incidentsHref, serverHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";

import { REFRESH_MS } from "./FleetOverview";
import { SeverityBadge } from "./IncidentBadges";
import styles from "./IncidentList.module.css";

/** Incidents per "load more". */
export const PAGE_SIZE = 50;
/** The API's largest page, which caps what a refresh reloads. */
const MAX_LIMIT = 500;

/** The incidents list for the page's query string, which holds its filters. */
export function IncidentsView() {
  const router = useRouter();
  const search = useSearchParams().toString();
  const filters = useMemo(() => filtersFromQuery(new URLSearchParams(search)), [search]);
  const onFiltersChange = useCallback(
    (next: IncidentFilters) => router.replace(incidentsHref(next), { scroll: false }),
    [router],
  );
  // New filters start from the first page instead of extending the old list.
  return <IncidentList key={search} filters={filters} onFiltersChange={onFiltersChange} />;
}

interface State {
  incidents: Incident[] | null;
  nextCursor: string | null;
  updatedAt: Date | null;
  error: string | null;
}

interface Props {
  filters: IncidentFilters;
  onFiltersChange: (filters: IncidentFilters) => void;
}

export function IncidentList({ filters, onFiltersChange }: Props) {
  const [state, setState] = useState<State>({ incidents: null, nextCursor: null, updatedAt: null, error: null });
  const [loadingMore, setLoadingMore] = useState(false);
  // How many incidents are shown, so a refresh reloads all of them at once
  // rather than collapsing the list back to its first page.
  const shown = useRef(PAGE_SIZE);

  const load = useCallback(
    async (signal: AbortSignal) => {
      const limit = Math.min(MAX_LIMIT, Math.max(PAGE_SIZE, shown.current));
      try {
        const page = await api.incidents({ ...apiQuery(filters), limit }, signal);
        setState({ incidents: page.incidents, nextCursor: page.next_cursor, updatedAt: new Date(), error: null });
      } catch (e) {
        // A 401 has already been routed to the login page.
        if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) {
          setState((s) => ({ ...s, error: describeError(e) }));
        }
      }
    },
    [filters],
  );
  usePolling(load, REFRESH_MS);

  async function loadMore() {
    if (!state.nextCursor) return;
    setLoadingMore(true);
    try {
      const page = await api.incidents({ ...apiQuery(filters), limit: PAGE_SIZE, cursor: state.nextCursor });
      setState((s) => {
        const incidents = [...(s.incidents ?? []), ...page.incidents];
        shown.current = incidents.length;
        return { ...s, incidents, nextCursor: page.next_cursor, error: null };
      });
    } catch (e) {
      if (!(e instanceof ApiError && e.status === 401)) {
        setState((s) => ({ ...s, error: describeError(e) }));
      }
    } finally {
      setLoadingMore(false);
    }
  }

  const { incidents, nextCursor, updatedAt, error } = state;
  const filtered = incidentsHref(filters) !== incidentsHref();

  return (
    <section aria-labelledby="incidents-title">
      <div className={styles.heading}>
        <h1 id="incidents-title">Incidents</h1>
        {updatedAt && (
          <p className={styles.meta}>
            Updated {updatedAt.toLocaleTimeString("en")}, refreshes every {REFRESH_MS / 1000} seconds.
          </p>
        )}
      </div>
      <Filters filters={filters} onChange={onFiltersChange} />
      {error && (
        <p className={styles.error} role="alert">
          {incidents ? `Refresh failed: ${error} Showing the last data received.` : error}
        </p>
      )}
      {!incidents && !error && <p role="status">Loading incidents…</p>}
      {incidents?.length === 0 && (
        <p className={styles.empty}>
          {filtered ? "No incidents match these filters." : "No incidents. Thresholds have not fired yet."}
        </p>
      )}
      {incidents && updatedAt && incidents.length > 0 && (
        <>
          <IncidentTable incidents={incidents} now={updatedAt} />
          {nextCursor && (
            <button className={styles.more} type="button" onClick={loadMore} disabled={loadingMore}>
              {loadingMore ? "Loading…" : "Load more"}
            </button>
          )}
        </>
      )}
    </section>
  );
}

function Filters({ filters, onChange }: { filters: IncidentFilters; onChange: (f: IncidentFilters) => void }) {
  const [sites, setSites] = useState<Site[]>([]);
  const [servers, setServers] = useState<Server[]>([]);

  // Options load once; a failure leaves the choices that are in the URL.
  useEffect(() => {
    const controller = new AbortController();
    api.sites(controller.signal).then((r) => setSites(r.sites), () => {});
    api.servers(controller.signal).then(
      (r) => setServers([...r.servers].sort((a, b) => a.hostname.localeCompare(b.hostname))),
      () => {},
    );
    return () => controller.abort();
  }, []);

  const set = (patch: Partial<IncidentFilters>) => onChange({ ...filters, ...patch });
  const siteOptions = withCurrent(sites.map((s) => [s.id, s.name]), filters.siteId, "Selected site");
  const serverOptions = withCurrent(
    servers.filter((s) => !filters.siteId || s.site_id === filters.siteId).map((s) => [s.id, s.hostname]),
    filters.serverId,
    "Selected server",
  );

  return (
    <form className={styles.filters} aria-label="Filters" onSubmit={(e) => e.preventDefault()}>
      <label>
        State
        <select value={filters.state} onChange={(e) => set({ state: e.target.value as IncidentFilters["state"] })}>
          <option value="all">All</option>
          <option value="open">Open</option>
          <option value="resolved">Resolved</option>
        </select>
      </label>
      <label>
        Severity
        <select
          value={filters.severity ?? ""}
          onChange={(e) => set({ severity: (e.target.value || null) as IncidentFilters["severity"] })}
        >
          <option value="">Any</option>
          <option value="warning">Warning</option>
          <option value="critical">Critical</option>
        </select>
      </label>
      <label>
        Site
        <select value={filters.siteId ?? ""} onChange={(e) => set({ siteId: e.target.value || null, serverId: null })}>
          <option value="">Any</option>
          {siteOptions.map(([id, name]) => (
            <option key={id} value={id}>
              {name}
            </option>
          ))}
        </select>
      </label>
      <label>
        Server
        <select value={filters.serverId ?? ""} onChange={(e) => set({ serverId: e.target.value || null })}>
          <option value="">Any</option>
          {serverOptions.map(([id, name]) => (
            <option key={id} value={id}>
              {name}
            </option>
          ))}
        </select>
      </label>
      {incidentsHref(filters) !== incidentsHref() && (
        <button className={styles.clear} type="button" onClick={() => onChange(NO_FILTERS)}>
          Clear filters
        </button>
      )}
    </form>
  );
}

/** `options`, plus the selected ID when the options do not include it (yet). */
function withCurrent(options: string[][], current: string | null, label: string): string[][] {
  return current && !options.some(([id]) => id === current) ? [...options, [current, label]] : options;
}

function IncidentTable({ incidents, now }: { incidents: Incident[]; now: Date }) {
  return (
    <div className={styles.scroll}>
      <table className={styles.table} aria-labelledby="incidents-title">
        <thead>
          <tr>
            <th scope="col">Severity</th>
            <th scope="col">Server</th>
            <th scope="col">Metric</th>
            <th scope="col">Opened</th>
            <th scope="col">State</th>
            <th scope="col">Duration</th>
            <th scope="col">Last value</th>
            <th scope="col">Peak</th>
          </tr>
        </thead>
        <tbody>
          {incidents.map((incident) => {
            const unit = unitOf(incident.metric);
            return (
              <tr key={incident.id}>
                <td>
                  <SeverityBadge severity={incident.severity} state={incident.state} />
                </td>
                <td>
                  <Link href={serverHref(incident.server_id)}>{incident.hostname}</Link>{" "}
                  <span className={styles.muted}>{incident.site_name}</span>
                </td>
                <th scope="row">
                  <Link href={incidentHref(incident.id)}>
                    {metricTitle(incident.metric)}
                    {incident.instance && <code className={styles.instance}>{incident.instance}</code>}
                  </Link>
                </th>
                <td>
                  <Time iso={incident.opened_at} now={now} />
                </td>
                <td>
                  {incident.resolved_at ? (
                    <>
                      Resolved <Time iso={incident.resolved_at} now={now} />
                    </>
                  ) : (
                    <strong>Open</strong>
                  )}
                </td>
                <td>{formatSeconds(incidentSeconds(incident.opened_at, incident.resolved_at, now))}</td>
                <td>{formatValue(incident.last_value, unit)}</td>
                <td>{formatValue(incident.peak_value, unit)}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}

export function Time({ iso, now }: { iso: string; now: Date }) {
  return (
    <time dateTime={iso} title={formatDateTime(iso)}>
      {formatRelative(iso, now)}
    </time>
  );
}
