"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { type Relay, api, describeError, isAbortError } from "@/lib/api";
import { formatDateTime, formatRelative } from "@/lib/format";
import { relayHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";

import styles from "./Admin.module.css";
import { AdminLayout } from "./AdminNav";
import { CertificateExpiry } from "./CertificateExpiry";
import { REFRESH_MS, StatusBadge } from "./FleetOverview";

/** `/admin/relays/`: every relay the user reads. */
export function RelayListPage() {
  return <AdminLayout>{() => <RelayList />}</AdminLayout>;
}

export function RelayList() {
  const [relays, setRelays] = useState<Relay[] | null>(null);
  const [updatedAt, setUpdatedAt] = useState<Date | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async (signal: AbortSignal) => {
    try {
      setRelays((await api.relays(signal)).relays);
      setUpdatedAt(new Date());
      setError(null);
    } catch (e) {
      if (!isAbortError(e)) setError(describeError(e));
    }
  }, []);
  usePolling(load, REFRESH_MS);

  return (
    <section aria-labelledby="relays-title">
      <h1 id="relays-title">Relays</h1>
      {error && (
        <p className={styles.error} role="alert">
          {relays ? `Refresh failed: ${error} Showing the last data received.` : error}
        </p>
      )}
      {!relays && !error && <p role="status">Loading relays…</p>}
      {relays?.length === 0 && (
        <p className={styles.empty}>No relays yet. Create a relay enrollment token to add one.</p>
      )}
      {relays && updatedAt && relays.length > 0 && (
        <div className={styles.scroll}>
          <table className={styles.table} aria-labelledby="relays-title">
            <thead>
              <tr>
                <th scope="col">Site</th>
                <th scope="col">Relay</th>
                <th scope="col">Status</th>
                <th scope="col">Agents</th>
                <th scope="col">Server names</th>
                <th scope="col">Certificate</th>
                <th scope="col">Last seen</th>
              </tr>
            </thead>
            <tbody>
              {relays.map((relay) => (
                <tr key={relay.id}>
                  <td>
                    {relay.site_name} <span className={styles.meta}>({relay.customer_name})</span>
                  </td>
                  <th scope="row">
                    <Link href={relayHref(relay.id)}>{relay.peer_id}</Link>
                  </th>
                  <td>
                    <StatusBadge server={relay} />
                  </td>
                  <td>
                    {relay.connected_agents} connected, {relay.server_count} attached
                  </td>
                  <td>{relay.server_names.join(", ") || "none"}</td>
                  <td>
                    {relay.cert_not_after ? (
                      <CertificateExpiry notAfter={relay.cert_not_after} now={updatedAt} />
                    ) : (
                      "none"
                    )}
                  </td>
                  <td>
                    {relay.last_heartbeat ? (
                      <time dateTime={relay.last_heartbeat} title={formatDateTime(relay.last_heartbeat)}>
                        {formatRelative(relay.last_heartbeat, updatedAt)}
                      </time>
                    ) : (
                      "never"
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}
