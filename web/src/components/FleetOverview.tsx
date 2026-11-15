"use client";

import Link from "next/link";
import { useCallback } from "react";

import { type Incident, type Server, type ServerState, api, describeError, isUnauthorized, isAbortError } from "@/lib/api";
import { certificateProblem, certificateState } from "@/lib/certificates";
import { formatOs } from "@/lib/format";
import { incidentTitle, isOffline } from "@/lib/incidents";
import { administersCustomer, multiCustomer, useMe } from "@/lib/me";
import { incidentsHref, serverHref, TOKENS_PATH } from "@/lib/routes";
import { type Scope, scopedHref, useScope } from "@/lib/scope";
import { formatSensorValue } from "@/lib/sensors";
import { isServerState, SERVER_STATES, STATE_TONES, serverState, stateWord, stateHref } from "@/lib/serverState";
import { REFRESH_MS, usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { Button, ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell, useUrlSort } from "@/components/ui/DataTable";
import { FilterBar, NoMatches } from "@/components/ui/FilterBar";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatDay, formatDuration, formatExact, formatSince } from "@/components/ui/time";

import styles from "./FleetOverview.module.css";
import { FleetTree, type TreeRow } from "./FleetTree";

// Other pages import the interval from here.
export { REFRESH_MS };

/** A server and what the fleet table says about it. */
interface Row extends TreeRow {
  server: Server;
  /** Its worst open incident, when the incidents list loaded. */
  incident: Incident | undefined;
  critical: number;
  warning: number;
  state: ServerState;
  /** The "Problems first" order, see `RANK`; `HEALTHY` and above fold away. */
  rank: number;
}

/**
 * "Problems first": what needs a look, then a late certificate on a server
 * otherwise fine, then what someone is on or silenced, then the rest.
 */
const RANK: Record<ServerState, number> = { critical: 0, warning: 1, offline: 2, acknowledged: 4, silenced: 5, ok: 6 };
const CERTIFICATE_RANK = 3;
const HEALTHY = RANK.ok;

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
    const state = serverState(server);
    const certificateWarning = server.cert_not_after !== null && certificateState(server.cert_not_after, now) !== "valid";
    const rank = state === "ok" && certificateWarning ? CERTIFICATE_RANK : RANK[state];
    const what = incident
      ? `${incidentTitle(incident)}${isOffline(incident) ? "" : ` ${formatSensorValue(incident.metric, incident.last_value)}`}`
      : stateWord(state);
    return {
      key: server.id,
      customer: { id: server.customer_id, name: server.customer_name },
      site: { id: server.site_id, name: server.site_name },
      healthy: rank === HEALTHY,
      problem: `${server.hostname} · ${what}`,
      href: serverHref(server.id),
      server,
      incident,
      critical,
      warning,
      state,
      rank,
    };
  });
}

const byName = new Intl.Collator("en", { numeric: true });
const problemsFirst = (a: Row, b: Row) => a.rank - b.rank || byName.compare(a.server.hostname, b.server.hostname);

/** The fleet: filters, then every server by site, problems first. Refreshes every 15 s. */
export function FleetOverview() {
  const scope = useScope();
  const me = useMe();
  const [params] = useUrlParams();
  const [sort, setSort] = useUrlSort();
  const fetcher = useCallback((signal: AbortSignal) => loadFleet(signal), []);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);

  const now = updatedAt ?? new Date();
  const all = data ? toRows({ ...data, servers: data.servers.filter((s) => inScope(s, scope)) }, now) : [];
  const online = all.filter((r) => r.server.status === "online").length;

  // `status` (online, offline) is kept for older links.
  const status = params.get("status");
  const state = params.get("state");
  const query = params.get("q")?.trim().toLowerCase() ?? "";
  const filtered = all.filter(
    (r) =>
      (!status || r.server.status === status) &&
      (!isServerState(state) || r.state === state) &&
      (!scope.siteId && params.get("site") ? r.server.site_id === params.get("site") : true) &&
      (!params.get("os") || r.server.os_family === params.get("os")) &&
      (params.get("incidents") !== "1" || r.critical + r.warning > 0) &&
      (!query || `${r.server.hostname} ${r.server.peer_id}`.toLowerCase().includes(query)),
  );
  const anyFilter = ["state", "status", "site", "os", "incidents", "q"].some((k) => params.get(k));

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
      header: "Server",
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
      cell: (r) => <Label tone={STATE_TONES[r.state]}>{stateWord(r.state)}</Label>,
      sortValue: (r) => SERVER_STATES.indexOf(r.state),
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
          Paste one install command on your servers: each appears here under its name as soon as it connects.
        </EmptyState>
      ) : (
        <>
          <FilterBar
            search={{ name: "q", label: "Search", placeholder: "Search servers…", width: 240 }}
            filters={[
              {
                name: "state",
                label: "Status",
                options: SERVER_STATES.map((value) => ({ value, label: stateWord(value) })),
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
          {!data ? (
            <DataTable label="Servers" columns={columns} rows={[]} rowKey={(r) => r.key} loading />
          ) : filtered.length === 0 ? (
            <NoMatches names={["q", "state", "status", "os", "incidents", ...(scope.siteId ? [] : ["site"])]}>
              No servers match these filters.
            </NoMatches>
          ) : (
            <FleetTree
              rows={filtered}
              columns={columns}
              sort={sort}
              onSortChange={setSort}
              defaultOrder={problemsFirst}
              multiCustomer={multiCustomer(me)}
              filtered={anyFilter}
              groupedOut={["site"]}
              stateHref={(state, place) =>
                "siteId" in place
                  ? scopedHref(`${stateHref(state)}&site=${place.siteId}`, scope)
                  : scopedHref(stateHref(state), { customerId: place.customerId, siteId: null })
              }
            />
          )}
          {data && anyFilter && filtered.length > 0 && (
            <p className={styles.summary}>
              {filtered.length} of {all.length} shown
            </p>
          )}
        </>
      )}
    </Page>
  );
}

/**
 * "Disk used /var 96.8% · 4 h 20 min", or why else the server needs a look;
 * with its severity when the status does not say it (acknowledged, silenced).
 */
function Problem({ row, now, scope }: { row: Row; now: Date; scope: Scope }) {
  const note = whoAndWhy(row, now);
  if (!note) return <What row={row} now={now} scope={scope} />;
  return (
    <span className={styles.stack}>
      {row.incident && <What row={row} now={now} scope={scope} />}
      <span className={styles.note}>{note}</span>
    </span>
  );
}

/**
 * "Silenced by alice until 18:00 today: migration", "Acknowledged by bob
 * 14:02: replacing the disk": who took a silenced or acknowledged server
 * out of the problems, and why (ADR-0130).
 */
function whoAndWhy(row: Pick<Row, "server" | "state">, now: Date): string | null {
  const { silence, acknowledgement } = row.server;
  if (row.state === "silenced" && silence) {
    return `Silenced by ${silence.by ?? "someone"} until ${formatDay(new Date(silence.until), now)}: ${silence.reason}`;
  }
  if (row.state === "acknowledged" && acknowledgement) {
    const said = acknowledgement.note ? `: ${acknowledgement.note}` : "";
    return `Acknowledged by ${acknowledgement.by} ${formatSince(new Date(acknowledgement.at), now)}${said}`;
  }
  return null;
}

/** The problem itself: "Disk used /var 96.8% · 4 h 20 min". */
function What({ row, now, scope }: { row: Row; now: Date; scope: Scope }) {
  const { server, incident } = row;
  if (incident) {
    return (
      <span className={styles.problem}>
        {row.state !== incident.severity && !(row.state === "offline" && isOffline(incident)) && (
          <>
            <StatusLabel kind="severity" value={incident.severity} />
            &nbsp;{" "}
          </>
        )}
        {incidentTitle(incident)}{" "}
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
        Last contact {formatSince(new Date(server.last_heartbeat), now).replace(/^Yesterday/, "yesterday")}
      </span>
    );
  }
  const certificate = server.cert_not_after && certificateProblem(server.cert_not_after, now);
  if (server.cert_not_after && certificate) {
    const failed = certificateState(server.cert_not_after, now) === "failed";
    return (
      <span className={styles.problem}>
        <Label tone={failed ? "bad" : "warn"}>{failed ? "Critical" : "Warning"}</Label>
        &nbsp; Certificate: {certificate} <span>{formatExact(new Date(server.cert_not_after)).slice(0, 10)}</span>
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
