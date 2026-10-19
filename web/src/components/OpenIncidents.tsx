"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { ApiError, type Incident, api, describeError, isAbortError } from "@/lib/api";
import { formatValue, unitOf } from "@/lib/format";
import { metricTitle } from "@/lib/metrics";
import { incidentHref, incidentsHref } from "@/lib/routes";
import { usePolling } from "@/lib/usePolling";

import { REFRESH_MS } from "./FleetOverview";
import { SeverityBadge } from "./IncidentBadges";
import { Time } from "./IncidentList";
import styles from "./OpenIncidents.module.css";

/** Open incidents shown on a server page; the list page has the rest. */
export const SHOWN = 10;

interface State {
  incidents: Incident[] | null;
  more: boolean;
  updatedAt: Date | null;
  error: string | null;
}

/** The server's open incidents, linking to their pages. */
export function OpenIncidents({ serverId }: { serverId: string }) {
  const [state, setState] = useState<State>({ incidents: null, more: false, updatedAt: null, error: null });

  const load = useCallback(
    async (signal: AbortSignal) => {
      try {
        const page = await api.incidents({ state: "open", server_id: serverId, limit: SHOWN }, signal);
        setState({ incidents: page.incidents, more: page.next_cursor !== null, updatedAt: new Date(), error: null });
      } catch (e) {
        // A 401 has already been routed to the login page.
        if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) {
          setState((s) => ({ ...s, error: describeError(e) }));
        }
      }
    },
    [serverId],
  );
  usePolling(load, REFRESH_MS);

  const { incidents, more, updatedAt, error } = state;
  return (
    <section className={styles.section} aria-labelledby="open-incidents-title">
      <h2 id="open-incidents-title">Open incidents</h2>
      {error && (
        <p className={styles.error} role="alert">
          {incidents ? `Refresh failed: ${error} Showing the last data received.` : error}
        </p>
      )}
      {!incidents && !error && <p role="status">Loading incidents…</p>}
      {incidents?.length === 0 && <p className={styles.muted}>No open incidents.</p>}
      {incidents && updatedAt && incidents.length > 0 && (
        <ul className={styles.list}>
          {incidents.map((incident) => (
            <li key={incident.id}>
              <SeverityBadge severity={incident.severity} />
              <Link href={incidentHref(incident.id)}>
                {metricTitle(incident.metric)}
                {incident.instance && ` ${incident.instance}`}
              </Link>
              <span>{formatValue(incident.last_value, unitOf(incident.metric))}</span>
              <span className={styles.muted}>
                opened <Time iso={incident.opened_at} now={updatedAt} />
              </span>
            </li>
          ))}
        </ul>
      )}
      <p>
        <Link href={incidentsHref({ serverId })}>
          {more ? "More open incidents and the history" : "Incident history"}
        </Link>
      </p>
    </section>
  );
}
