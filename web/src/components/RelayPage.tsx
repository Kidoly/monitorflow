"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type ReactNode, useCallback, useState } from "react";

import { ApiError, type Me, type RelayDetail, api, describeError, isAbortError } from "@/lib/api";
import { formatDateTime, formatRelative } from "@/lib/format";
import { administersCustomer } from "@/lib/me";
import { RELAYS_PATH, serverHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";

import styles from "./Admin.module.css";
import { AdminLayout } from "./AdminNav";
import { CertificateExpiry } from "./CertificateExpiry";
import { REFRESH_MS, StatusBadge } from "./FleetOverview";
import { RevokeControl } from "./RevokeControl";

/** `/admin/relays/view/?id=…` (ADR-0020 routing). */
export function RelayView() {
  const id = useSearchParams().get("id");
  return <AdminLayout current="Relays">{(me) => <RelayPage key={id} id={id} me={me} />}</AdminLayout>;
}

export function RelayPage({ id, me }: { id: string | null; me: Me }) {
  const valid = id !== null && UUID.test(id);
  /** Undefined until loaded, null when not found or out of scope. */
  const [relay, setRelay] = useState<RelayDetail | null | undefined>(undefined);
  const [updatedAt, setUpdatedAt] = useState(new Date());
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      try {
        setRelay(await api.relay(id ?? "", signal));
        setUpdatedAt(new Date());
        setError(null);
      } catch (e) {
        if (isAbortError(e)) return;
        if (e instanceof ApiError && e.status === 404) setRelay(null);
        else setError(describeError(e));
      }
    },
    [id],
  );
  usePolling(load, REFRESH_MS, valid);

  if (!valid || relay === null) {
    return (
      <section className={styles.empty}>
        <h1>Relay not found</h1>
        <p>It may not exist, or your roles do not cover its site.</p>
        <Link href={RELAYS_PATH}>Back to relays</Link>
      </section>
    );
  }
  if (!relay) {
    return error ? (
      <p className={styles.error} role="alert">
        {error}
      </p>
    ) : (
      <p role="status">Loading relay…</p>
    );
  }
  const { certificate } = relay;
  return (
    <article aria-labelledby="relay-title">
      <p className={styles.meta}>
        <Link href={RELAYS_PATH}>Relays</Link> / {relay.customer_name} / {relay.site_name}
      </p>
      <h1 id="relay-title">{relay.peer_id}</h1>
      {error && (
        <p className={styles.error} role="alert">
          Refresh failed: {error} Showing the last data received.
        </p>
      )}
      <dl className={styles.facts}>
        <Fact term="Status">
          <StatusBadge server={relay} />
        </Fact>
        <Fact term="Site">{relay.site_name}</Fact>
        <Fact term="Last seen">
          {relay.last_heartbeat ? formatRelative(relay.last_heartbeat, updatedAt) : "never"}
        </Fact>
        <Fact term="Server names">{relay.server_names.join(", ") || "none"}</Fact>
        <Fact term="Certificate expires">
          {relay.cert_not_after ? <CertificateExpiry notAfter={relay.cert_not_after} now={updatedAt} /> : "no valid certificate"}
        </Fact>
        {certificate?.revoked && (
          <Fact term="Certificate revoked">
            <span className={styles.revoked}>
              {certificate.revoked_at ? formatDateTime(certificate.revoked_at) : "Yes"}
              {certificate.revocation_reason && ` (${certificate.revocation_reason})`}
            </span>
          </Fact>
        )}
        <Fact term="Enrolled">{formatDateTime(relay.created_at)}</Fact>
        {relay.revoked_at && (
          <Fact term="Revoked">
            <span className={styles.revoked}>{formatDateTime(relay.revoked_at)}</span>
          </Fact>
        )}
      </dl>
      {!relay.revoked_at && administersCustomer(me, relay.customer_id) && (
        <RevokeControl kind="relays" id={relay.id} name={relay.peer_id} onRevoked={() => void load()} />
      )}
      <section aria-labelledby="attached-title">
        <h2 id="attached-title">Attached agents</h2>
        <p className={styles.meta}>
          Servers whose latest connection came through this relay; {relay.connected_agents} connected now.
        </p>
        {relay.servers.length === 0 ? (
          <p className={styles.empty}>No agent has connected through this relay yet.</p>
        ) : (
          <ul>
            {relay.servers.map((server) => (
              <li key={server.id}>
                <Link href={serverHref(server.id)}>{server.hostname}</Link> <StatusBadge server={server} />
              </li>
            ))}
          </ul>
        )}
      </section>
    </article>
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
