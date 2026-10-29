"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { type Incident, type Server, api, describeError, isUnauthorized, isAbortError } from "@/lib/api";
import { certificateState } from "@/lib/certificates";
import { formatOs } from "@/lib/format";
import { incidentTitle, isOffline } from "@/lib/incidents";
import { administersCustomer, useMe } from "@/lib/me";
import { incidentsHref, serverHref, TOKENS_PATH } from "@/lib/routes";
import { type Scope, scopedHref, useScope } from "@/lib/scope";
import { formatSensorValue } from "@/lib/sensors";
import { REFRESH_MS, usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { Button, ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell, useUrlSort } from "@/components/ui/DataTable";
import { FilterBar } from "@/components/ui/FilterBar";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { problemRank } from "@/components/ui/problemsFirst";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { StatTiles } from "@/components/ui/StatTiles";
import { formatDuration, formatExact, formatShortDate, formatSince } from "@/components/ui/time";

import styles from "./FleetOverview.module.css";

// Other pages import the interval from here.
export { REFRESH_MS };

/** Healthy servers shown under the problems before "Show N more healthy servers". */
export const HEALTHY_SHOWN = 5;

/** A server and what the fleet table says about it. */
interface Row {
  server: Server;
  /** Its worst open incident, when the incidents list loaded. */
  incident: Incident | undefined;
  critical: number;
  warning: number;
  revoked: boolean;
  /** 0 open critical … 4 healthy, 5 revoked: the "Problems first" order. */
  rank: number;
}

interface Fleet {
  servers: Server[];
  /** Null when the incidents could not be read: the table falls back to the servers' counts. */
  incidents: Incident[] | null;
}

async function loadFleet(signal: AbortSignal): Promise<Fleet> {
  const incidents = api.incidents({ state: "open", limit: 500 }, signal).then(
    (page) => page.incidents,
    (e: unknown) => {
      if (isAbortError(e)) throw e;
      return null;
    },
  );
  const [{ servers }, open] = await Promise.all([api.servers(signal), incidents]);
  return { servers, incidents: open };
}

function inScope(item: { customer_id?: string; site_id: string }, scope: Scope): boolean {
  return (!scope.customerId || item.customer_id === scope.customerId) && (!scope.siteId || item.site_id === scope.siteId);
}

function toRows(fleet: Fleet, now: Date): Row[] {
  return fleet.servers.map((server) => {
    const incidents = fleet.incidents?.filter((i) => i.server_id === server.id) ?? [];
    const critical = incidents.filter((i) => i.severity === "critical").length;
    const warning = fleet.incidents ? incidents.length - critical : server.open_incidents;
    const incident = incidents.find((i) => i.severity === "critical") ?? incidents[0];
    const revoked = server.revoked_at !== null;
    const rank = revoked
      ? 5
      : problemRank({
          critical,
          warning,
          offline: server.status === "offline",
          certificateWarning: server.cert_not_after !== null && certificateState(server.cert_not_after, now) !== "valid",
          name: server.hostname,
        });
    return { server, incident, critical, warning, revoked, rank };
  });
}

const byName = new Intl.Collator("en", { numeric: true });
const problemsFirst = (a: Row, b: Row) => a.rank - b.rank || byName.compare(a.server.hostname, b.server.hostname);

/** The fleet: counters, filters and every server, problems first. Refreshes every 15 s. */
export function FleetOverview() {
  const scope = useScope();
  const me = useMe();
  const [params] = useUrlParams();
  const [sort, setSort] = useUrlSort();
  const [expanded, setExpanded] = useState(false);
  const fetcher = useCallback((signal: AbortSignal) => loadFleet(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);

  const now = updatedAt ?? new Date();
  const all = data ? toRows({ ...data, servers: data.servers.filter((s) => inScope(s, scope)) }, now) : [];
  // Servers' and relays' incidents: a relay offline counts too (ADR-0057).
  const ids = new Set(all.map((r) => r.server.id));
  const incidents = data?.incidents?.filter((i) => (i.server_id ? ids.has(i.server_id) : inScope(i, scope))) ?? [];
  const online = all.filter((r) => r.server.status === "online" && !r.revoked).length;

  const status = params.get("status");
  const query = params.get("q")?.trim().toLowerCase() ?? "";
  const filtered = all.filter(
    (r) =>
      (!status || (r.revoked ? "revoked" : r.server.status) === status) &&
      (!scope.siteId && params.get("site") ? r.server.site_id === params.get("site") : true) &&
      (!params.get("os") || r.server.os_family === params.get("os")) &&
      (params.get("incidents") !== "1" || r.critical + r.warning > 0) &&
      (!query || `${r.server.hostname} ${r.server.peer_id}`.toLowerCase().includes(query)),
  );
  const anyFilter = ["status", "site", "os", "incidents", "q"].some((k) => params.get(k));
  // Healthy rows fold away only in the default view: problems first, unfiltered.
  const collapsible = sort === null && !anyFilter;
  const ordered = [...filtered].sort(problemsFirst);
  const problems = ordered.filter((r) => r.rank < 4);
  const shown = collapsible && !expanded ? ordered.slice(0, problems.length + HEALTHY_SHOWN) : ordered;
  const hidden = ordered.length - shown.length;

  const sites = [...new Map(all.map((r) => [r.server.site_id, r.server.site_name])).entries()].sort((a, b) =>
    byName.compare(a[1], b[1]),
  );
  const families = [...new Set(all.map((r) => r.server.os_family))].sort();
  const canEnroll = administersCustomer(me, scope.customerId ?? undefined);
  const enroll = canEnroll && (
    <ButtonLink variant="k" href={scopedHref(TOKENS_PATH, scope)}>
      Add servers
    </ButtonLink>
  );

  const columns: Column<Row>[] = [
    {
      key: "host",
      header: "Host",
      cell: (r) => (
        <PrimaryCell
          title={r.server.hostname}
          href={scopedHref(serverHref(r.server.id), scope)}
          sub={[
            formatOs(r.server.os_family, r.server.os_version),
            `agent ${r.server.agent_version ?? "unknown"}`,
            r.server.via_relay && `via ${r.server.via_relay}`,
          ]
            .filter(Boolean)
            .join(" · ")}
        />
      ),
      sortValue: (r) => r.server.hostname,
    },
    {
      key: "status",
      header: "Status",
      cell: (r) => <StatusLabel kind="server" value={r.revoked ? "revoked" : r.server.status} />,
      sortValue: (r) => (r.revoked ? "revoked" : r.server.status),
    },
    { key: "problem", header: "Problem", cell: (r) => <Problem row={r} now={now} scope={scope} /> },
    { key: "site", header: "Site", cell: (r) => r.server.site_name, sortValue: (r) => r.server.site_name },
    {
      key: "seen",
      header: "Last seen",
      className: styles.muted,
      cell: (r) => (r.server.last_heartbeat ? <RelativeTime value={r.server.last_heartbeat} now={now} stale={stale} /> : "never"),
      sortValue: (r) => r.server.last_heartbeat,
    },
  ];

  return (
    <Page>
      <PageHeader
        title="Fleet"
        meta={
          <>
            {data && `${all.length} ${all.length === 1 ? "server" : "servers"}, ${online} online · `}
            <RefreshLine updatedAt={updatedAt} stale={stale} />
          </>
        }
        actions={
          <>
            <Button onClick={refresh}>Refresh now</Button>
            {enroll}
          </>
        }
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what="servers" error={describeError(error)} onRetry={refresh} />
      ) : data && all.length === 0 ? (
        <EmptyState title="No servers yet" actions={enroll}>
          Paste one install command on your hosts: each appears here under its name as soon as it connects.
        </EmptyState>
      ) : (
        <>
          <StatTiles
            tiles={[
              {
                label: "Online",
                value: online,
                total: all.length,
                tone: "ok",
                href: scopedHref(status === "online" ? "/" : "/?status=online", scope),
                active: status === "online",
              },
              {
                label: "Critical incidents",
                value: incidents.filter((i) => i.severity === "critical").length,
                tone: "bad",
                href: scopedHref(incidentsHref({ state: "open", severity: "critical" }), scope),
              },
              {
                label: "Warnings",
                value: incidents.filter((i) => i.severity === "warning").length,
                tone: "warn",
                href: scopedHref(incidentsHref({ state: "open", severity: "warning" }), scope),
              },
            ]}
          />
          <FilterBar
            search={{ name: "q", label: "Search", placeholder: "Search hosts…", width: 240 }}
            filters={[
              {
                name: "status",
                label: "Status",
                options: [
                  { value: "online", label: "Online" },
                  { value: "offline", label: "Offline" },
                  { value: "revoked", label: "Revoked" },
                ],
              },
              ...(scope.siteId ? [] : [{ name: "site", label: "Site", options: sites.map(([value, label]) => ({ value, label })) }]),
              { name: "os", label: "OS", options: families.map((f) => ({ value: f, label: formatOs(f, null) })) },
              { kind: "toggle" as const, name: "incidents", label: "Has incidents" },
            ]}
            trailing={
              <Button onClick={() => setSort(null)}>
                Problems first
              </Button>
            }
          />
          {data && filtered.length === 0 ? (
            <EmptyState>No servers match these filters.</EmptyState>
          ) : (
            <DataTable
              label="Servers"
              columns={columns}
              rows={shown}
              rowKey={(r) => r.server.id}
              sort={sort}
              onSortChange={setSort}
              defaultOrder={problemsFirst}
              rowMuted={(r) => r.revoked}
              loading={!data}
            />
          )}
          {data && filtered.length > 0 && (
            <p className={styles.summary}>
              {shown.length} of {all.length} shown
              {collapsible && ordered.length > problems.length + HEALTHY_SHOWN && (
                <>
                  {" · "}
                  <button type="button" className={styles.linkButton} onClick={() => setExpanded(!expanded)}>
                    {expanded ? "Fold healthy servers" : `Show ${hidden} more healthy ${hidden === 1 ? "server" : "servers"}`}
                  </button>
                </>
              )}
            </p>
          )}
        </>
      )}
    </Page>
  );
}

/** "Critical  Disk used /var 96.8% · 4 h 20 min", or why else the server needs a look. */
function Problem({ row, now, scope }: { row: Row; now: Date; scope: Scope }) {
  const { server, incident } = row;
  if (row.revoked) {
    return <span className={styles.muted}>Revoked on {formatShortDate(new Date(server.revoked_at!))}</span>;
  }
  if (incident) {
    return (
      <span className={styles.problem}>
        <StatusLabel kind="severity" value={incident.severity} />
        &nbsp; {incidentTitle(incident)}{" "}
        <span>
          {!isOffline(incident) && `${formatSensorValue(incident.metric, incident.last_value)} · `}
          {formatDuration((now.getTime() - Date.parse(incident.opened_at)) / 1000)}
        </span>
      </span>
    );
  }
  if (row.warning > 0) {
    // The incidents list did not load; the server's own count still links to them.
    return (
      <Link href={scopedHref(incidentsHref({ state: "open", serverId: server.id }), scope)} className={styles.problem}>
        {row.warning} open {row.warning === 1 ? "incident" : "incidents"}
      </Link>
    );
  }
  if (server.status === "offline") {
    if (!server.last_heartbeat) return <span className={styles.muted}>Never connected</span>;
    return (
      <span className={styles.muted}>
        No heartbeat since {formatSince(new Date(server.last_heartbeat), now).replace(/^Yesterday/, "yesterday")}
      </span>
    );
  }
  if (server.cert_not_after && certificateState(server.cert_not_after, now) !== "valid") {
    const expiry = new Date(server.cert_not_after);
    const expired = expiry <= now;
    const days = Math.round(Math.abs(expiry.getTime() - now.getTime()) / 86_400_000);
    return (
      <span className={styles.problem}>
        <Label tone={expired ? "bad" : "warn"}>{expired ? "Critical" : "Warning"}</Label>
        &nbsp; {expired ? `Certificate expired ${days} ${days === 1 ? "day" : "days"} ago` : `Certificate expires in ${days} ${days === 1 ? "day" : "days"}`}{" "}
        <span>{formatExact(expiry).slice(0, 10)}</span>
      </span>
    );
  }
  return <span className={styles.muted}>—</span>;
}

/** Live status of a relay or attached agent, for the relay pages until their redesign. */
export function StatusBadge({
  server,
}: {
  server: Pick<Server, "status"> & { via_relay?: string | null; revoked_at?: string | null };
}) {
  const online = server.status === "online";
  return (
    <span className={online ? styles.online : styles.offline}>
      <span aria-hidden="true">{online ? "●" : "○"}</span> {online ? "Online" : "Offline"}
      {server.via_relay && ` via ${server.via_relay}`}
      {server.revoked_at && ", revoked"}
    </span>
  );
}
