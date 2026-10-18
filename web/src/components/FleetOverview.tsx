"use client";

import { useEffect, useState } from "react";

import { type Server, api, describeError, isAbortError } from "@/lib/api";
import { formatDateTime, formatOs, formatRelative } from "@/lib/format";

import styles from "./FleetOverview.module.css";

export const REFRESH_MS = 15_000;

interface State {
  servers: Server[] | null;
  /** When `servers` was fetched. */
  updatedAt: Date | null;
  error: string | null;
}

/** The fleet table, refreshed every {@link REFRESH_MS}. */
export function FleetOverview() {
  const [state, setState] = useState<State>({ servers: null, updatedAt: null, error: null });

  useEffect(() => {
    let controller: AbortController | undefined;
    async function load() {
      controller?.abort();
      controller = new AbortController();
      try {
        const { servers } = await api.servers(controller.signal);
        setState({ servers, updatedAt: new Date(), error: null });
      } catch (e) {
        if (!isAbortError(e)) {
          setState((s) => ({ ...s, error: describeError(e) }));
        }
      }
    }
    void load();
    const timer = setInterval(load, REFRESH_MS);
    return () => {
      clearInterval(timer);
      controller?.abort();
    };
  }, []);

  const { servers, updatedAt, error } = state;
  const online = servers?.filter((s) => s.status === "online").length ?? 0;

  return (
    <section aria-labelledby="fleet-title">
      <div className={styles.heading}>
        <h1 id="fleet-title">Fleet</h1>
        {servers && updatedAt && (
          <p className={styles.meta}>
            {servers.length} {servers.length === 1 ? "server" : "servers"}, {online} online.
            Updated {updatedAt.toLocaleTimeString("en")}, refreshes every {REFRESH_MS / 1000} seconds.
          </p>
        )}
      </div>
      {error && (
        <p className={styles.error} role="alert">
          {servers ? `Refresh failed: ${error} Showing the last data received.` : error}
        </p>
      )}
      {!servers && !error && <p role="status">Loading servers…</p>}
      {servers?.length === 0 && (
        <p className={styles.empty}>
          No servers yet. Enroll an agent with a token from{" "}
          <code>monitorflow-server token create</code>.
        </p>
      )}
      {servers && updatedAt && servers.length > 0 && (
        <ServerTable servers={servers} now={updatedAt} />
      )}
    </section>
  );
}

function ServerTable({ servers, now }: { servers: Server[]; now: Date }) {
  return (
    <div className={styles.scroll}>
      <table className={styles.table} aria-labelledby="fleet-title">
        <thead>
          <tr>
            <th scope="col">Site</th>
            <th scope="col">Hostname</th>
            <th scope="col">OS</th>
            <th scope="col">Agent</th>
            <th scope="col">Status</th>
            <th scope="col">Last seen</th>
          </tr>
        </thead>
        <tbody>
          {servers.map((server) => (
            <tr key={server.id}>
              <td>{server.site_name}</td>
              <th scope="row">{server.hostname}</th>
              <td>{formatOs(server.os_family, server.os_version)}</td>
              <td>{server.agent_version ?? "unknown"}</td>
              <td>
                <StatusBadge server={server} />
              </td>
              <td>
                {server.last_heartbeat ? (
                  <time dateTime={server.last_heartbeat} title={formatDateTime(server.last_heartbeat)}>
                    {formatRelative(server.last_heartbeat, now)}
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
  );
}

function StatusBadge({ server }: { server: Server }) {
  const online = server.status === "online";
  return (
    <span className={online ? styles.online : styles.offline}>
      <span aria-hidden="true">{online ? "●" : "○"}</span> {online ? "Online" : "Offline"}
      {server.via_relay && ` via ${server.via_relay}`}
      {server.revoked_at && ", revoked"}
    </span>
  );
}
