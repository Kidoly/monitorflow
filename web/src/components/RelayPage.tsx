"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback } from "react";

import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { type Fact, FactsBox } from "@/components/ui/FactsBox";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { ErrorState, LoadingBlock, NotFound, PageHeader, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { StatusLabel } from "@/components/ui/StatusLabel";
import {
  ApiError,
  type AttachedServer,
  type Incident,
  type Me,
  type RelayCommand,
  type RelayDetail,
  type RevocationReason,
  type Server,
  api,
  describeError,
} from "@/lib/api";
import { updateAvailable } from "@/lib/agentVersion";
import { certificateState } from "@/lib/certificates";
import { formatDay, formatMinute, formatWhen } from "@/lib/clock";
import { formatOs, formatValue, unitOf } from "@/lib/format";
import { administersCustomer } from "@/lib/me";
import { metricTitle } from "@/lib/metrics";
import { RELAYS_PATH, serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";
import { OfflineAlerts } from "./OfflineAlerts";
import { CertificateCell, relayStatus } from "./RelayList";

/** RFC 5280 reasons, as the revoke route takes them (ADR-0029). */
const REASONS: { value: RevocationReason; label: string }[] = [
  { value: "cessation_of_operation", label: "Decommissioned" },
  { value: "key_compromise", label: "Key compromised" },
  { value: "affiliation_changed", label: "Moved to another organisation" },
  { value: "superseded", label: "Replaced by a new enrollment" },
  { value: "unspecified", label: "Unspecified" },
];

/** Open incidents fetched to fill the Problem column; the API has no per-relay filter. */
const INCIDENT_LIMIT = 500;

/** `/admin/relays/view/?id=…` (ADR-0020 routing). */
export function RelayView() {
  const id = useSearchParams().get("id");
  return <AdminLayout>{(me) => <RelayPage key={id} id={id} me={me} />}</AdminLayout>;
}

/** An attached server with what the servers and incidents lists add to it. */
interface Row extends AttachedServer {
  server?: Server;
  /** Its open incidents, worst first. */
  incidents: Incident[];
}

interface Data {
  relay: RelayDetail;
  rows: Row[];
  /** The newest release the server holds. */
  latest: string | null;
}

const severityRank = (incident: Incident) => (incident.severity === "critical" ? 0 : 1);

async function load(id: string, signal: AbortSignal): Promise<Data | null> {
  let relay: RelayDetail;
  try {
    relay = await api.relay(id, signal);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
  const latest = await api.agentReleases(signal).then(
    (r) => r.latest,
    () => null,
  );
  if (relay.servers.length === 0) return { relay, rows: [], latest };
  const [{ servers }, { incidents }] = await Promise.all([
    api.servers(signal),
    api.incidents({ state: "open", limit: INCIDENT_LIMIT }, signal),
  ]);
  const rows = relay.servers.map((attached) => ({
    ...attached,
    server: servers.find((s) => s.id === attached.id),
    incidents: incidents.filter((i) => i.server_id === attached.id).sort((a, b) => severityRank(a) - severityRank(b)),
  }));
  return { relay, rows, latest };
}

const ACTIVE: RelayCommand["state"][] = ["queued", "sent", "running"];

/** The state of the relay's latest self-update, in a few words. */
export function updateText(update: RelayCommand): string {
  switch (update.state) {
    case "queued":
    case "sent":
      return `Update to ${update.version} queued`;
    case "running":
      return `Updating to ${update.version}`;
    case "succeeded":
      return `Updated to ${update.version}`;
    case "failed":
      return `Update to ${update.version} failed${update.error ? `: ${update.error}` : ""}`;
    default:
      return `Update to ${update.version} ${update.state}`;
  }
}

/**
 * "Update relay to 0.3.0…", for administrators of the relay's customer
 * when the server holds a newer release than the relay runs (ADR-0055).
 */
function UpdateRelay({ relay, latest, me, onQueued }: { relay: RelayDetail; latest: string | null; me: Me; onQueued: () => void }) {
  const busy = relay.last_update && ACTIVE.includes(relay.last_update.state);
  if (
    relay.revoked_at ||
    busy ||
    !latest ||
    !updateAvailable(relay.relay_version ?? null, latest) ||
    !administersCustomer(me, relay.customer_id)
  ) {
    return null;
  }
  return (
    <InlineConfirm
      trigger={`Update relay to ${latest}…`}
      triggerVariant="default"
      title={`Update ${relay.peer_id} to ${latest}?`}
      consequence={`The relay downloads ${latest}, checks its signature and restarts into it; its agents reconnect a few seconds later. If the new relay does not connect within 5 minutes, ${relay.relay_version ?? "the current version"} comes back.`}
      confirmLabel={`Update ${relay.peer_id}`}
      pendingLabel="Queuing…"
      onConfirm={async () => {
        await api.updateRelay(relay.id, latest);
        onQueued();
      }}
      describeError={(e) => `The update was not queued. ${describeError(e)}`}
    />
  );
}

/** The worst open incident, else why an offline server is quiet. */
function Problem({ row, now }: { row: Row; now: Date }) {
  const [worst, ...others] = row.incidents;
  if (worst) {
    return (
      <>
        <StatusLabel kind="severity" value={worst.severity} />
        &nbsp; {metricTitle(worst.metric)}
        {worst.instance && ` ${worst.instance}`}{" "}
        <span className={styles.dim}>{formatValue(worst.last_value, unitOf(worst.metric))}</span>
        {others.length > 0 && <span className={styles.dim}> · {others.length} more</span>}
      </>
    );
  }
  if (row.status === "offline") {
    const last = row.server?.last_heartbeat;
    return (
      <span className={styles.dim}>
        {last ? `No heartbeat since ${formatWhen(new Date(last), now)}` : "Never connected"}
      </span>
    );
  }
  return <span className={styles.dim}>—</span>;
}

export function RelayPage({ id, me }: { id: string | null; me: Me }) {
  const valid = id !== null && UUID.test(id);
  const fetcher = useCallback((signal: AbortSignal) => load(id ?? "", signal), [id]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });

  const crumb = (current: string) => <Breadcrumb parents={[{ label: "Relays", href: RELAYS_PATH }]} current={current} />;
  if (!valid || data === null) {
    return (
      <div className={styles.page}>
        {crumb("Not found")}
        <NotFound title="Relay not found" actions={<Link href={RELAYS_PATH}>Back to relays</Link>} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {crumb("Relay")}
        {error ? <ErrorState what="the relay" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock label="Loading relay" />}
      </div>
    );
  }

  const { relay, rows, latest } = data;
  const now = updatedAt ?? new Date();
  const { certificate } = relay;
  const facts: Fact[] = [
    { label: "Site", value: `${relay.site_name} · ${relay.customer_name}` },
    {
      label: "Last seen",
      value: relay.last_heartbeat ? <RelativeTime value={relay.last_heartbeat} now={now} /> : "never",
    },
    {
      label: "Connected since",
      value: relay.status === "online" && relay.connected_at ? formatMinute(new Date(relay.connected_at)) : "—",
    },
    {
      label: "Certificate",
      value: certificate?.revoked ? (
        <StatusLabel kind="certificate" value="revoked" />
      ) : relay.cert_not_after && certificateState(relay.cert_not_after, now) !== "valid" ? (
        <>
          <CertificateCell notAfter={relay.cert_not_after} now={now} /> {formatDay(new Date(relay.cert_not_after))}
        </>
      ) : (
        <CertificateCell notAfter={relay.cert_not_after} now={now} />
      ),
    },
    { label: "Version", value: relay.relay_version ?? "—" },
    ...(relay.last_update ? [{ label: "Self-update", value: updateText(relay.last_update) }] : []),
    { label: "Server names", value: relay.server_names.join(", ") || "—" },
    ...(certificate ? [{ label: "Certificate serial", value: certificate.serial, mono: true }] : []),
    { label: "Enrolled", value: formatMinute(new Date(relay.created_at)) },
    ...(relay.revoked_at
      ? []
      : [{ label: "Offline alerts", value: <OfflineAlerts kind="relays" target={relay} onChanged={refresh} /> }]),
    ...(relay.revoked_at
      ? [
          { label: "Revoked", value: formatMinute(new Date(relay.revoked_at)) },
          ...(certificate?.revocation_reason ? [
                {
                  label: "Reason",
                  value:
                    REASONS.find((r) => r.value === certificate.revocation_reason)?.label ??
                    certificate.revocation_reason,
                },
              ] : []),
        ]
      : []),
  ];

  const connected = rows.filter((row) => row.status === "online").length;
  const columns: Column<Row>[] = [
    {
      key: "host",
      header: "Host",
      cell: (row) => (
        <PrimaryCell
          title={row.hostname}
          href={serverHref(row.id)}
          sub={
            row.server &&
            [formatOs(row.server.os_family, row.server.os_version), row.server.agent_version && `agent ${row.server.agent_version}`]
              .filter(Boolean)
              .join(" · ")
          }
        />
      ),
    },
    { key: "status", header: "Status", cell: (row) => <StatusLabel kind="server" value={row.status} /> },
    { key: "problem", header: "Problem", cell: (row) => <Problem row={row} now={now} /> },
    {
      key: "seen",
      header: "Last seen",
      className: styles.dim,
      cell: (row) =>
        row.server?.last_heartbeat ? <RelativeTime value={row.server.last_heartbeat} now={now} /> : "never",
    },
  ];

  return (
    <div className={styles.page}>
      {crumb(relay.peer_id)}
      <PageHeader
        detail
        title={relay.peer_id}
        status={<StatusLabel kind="server" value={relayStatus(relay)} />}
        actions={
          !relay.revoked_at &&
          administersCustomer(me, relay.customer_id) && (
            <>
            <UpdateRelay relay={relay} latest={latest} me={me} onQueued={refresh} />
            <InlineConfirm
              trigger={`Revoke ${relay.peer_id}…`}
              title={`Revoke ${relay.peer_id}?`}
              consequence="Its certificates go into the revocation list, its connections close now, including the agents it forwards, and it cannot connect again. This cannot be undone: enrolling it again needs a new ID."
              reasons={REASONS}
              confirmLabel={`Revoke ${relay.peer_id}`}
              pendingLabel="Revoking…"
              onConfirm={async (reason) => {
                await api.revoke("relays", relay.id, reason ?? "unspecified");
                refresh();
              }}
              describeError={(e) => `Revocation failed. ${describeError(e)}`}
            />
            </>
          )
        }
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      <FactsBox columns={2} facts={facts} />
      <section className={styles.section} aria-labelledby="attached-title">
        <div className={styles.sectionHead}>
          <h2 id="attached-title" className={styles.sectionTitle}>
            Servers connecting through {relay.peer_id}
          </h2>
          <span className={styles.dim}>
            {rows.length} {rows.length === 1 ? "server" : "servers"}, {connected} connected now
          </span>
        </div>
        {rows.length === 0 ? (
          <p className={styles.sectionNote}>No agent has connected through this relay yet.</p>
        ) : (
          <DataTable bare label={`Servers connecting through ${relay.peer_id}`} columns={columns} rows={rows} rowKey={(row) => row.id} />
        )}
      </section>
    </div>
  );
}
