"use client";

import { useCallback, useState } from "react";

import {
  ApiError,
  type EffectiveSensors,
  type SensorOrigin,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { formatDateTime, formatRelative, formatSeconds } from "@/lib/format";
import { usePolling } from "@/lib/usePolling";

import { REFRESH_MS } from "./FleetOverview";
import styles from "./SensorConfigPanel.module.css";

const ORIGINS: Record<SensorOrigin, string> = {
  default: "Built-in default",
  site: "Site",
  server: "This server",
};

interface State {
  config: EffectiveSensors | null;
  updatedAt: Date | null;
  error: string | null;
}

/** The sensors the agent should run, where each comes from, and whether it has them. Read-only. */
export function SensorConfigPanel({
  serverId,
  onLoad,
}: {
  serverId: string;
  /** Called with each configuration received, so the page can share it. */
  onLoad?: (config: EffectiveSensors) => void;
}) {
  const [state, setState] = useState<State>({ config: null, updatedAt: null, error: null });

  const load = useCallback(
    async (signal: AbortSignal) => {
      try {
        const config = await api.serverSensors(serverId, signal);
        setState({ config, updatedAt: new Date(), error: null });
        onLoad?.(config);
      } catch (e) {
        // A 401 has already been routed to the login page.
        if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) {
          setState((s) => ({ ...s, error: describeError(e) }));
        }
      }
    },
    [serverId, onLoad],
  );
  usePolling(load, REFRESH_MS);

  const { config, updatedAt, error } = state;
  return (
    <section className={styles.section} aria-labelledby="sensors-title">
      <h2 id="sensors-title">Sensor configuration</h2>
      {error && (
        <p className={styles.error} role="alert">
          {config ? `Refresh failed: ${error} Showing the last data received.` : error}
        </p>
      )}
      {!config && !error && <p role="status">Loading sensor configuration…</p>}
      {config && updatedAt && (
        <>
          <Drift config={config} now={updatedAt} />
          {config.sensors.length === 0 ? (
            <p className={styles.empty}>No sensors: every default is turned off for this server.</p>
          ) : (
            <div className={styles.scroll}>
              <table className={styles.table} aria-labelledby="sensors-title">
                <thead>
                  <tr>
                    <th scope="col">Sensor</th>
                    <th scope="col">Instance</th>
                    <th scope="col">Kind</th>
                    <th scope="col">Interval</th>
                    <th scope="col">Timeout</th>
                    <th scope="col">Parameters</th>
                    <th scope="col">Origin</th>
                  </tr>
                </thead>
                <tbody>
                  {config.sensors.map((sensor) => (
                    <tr key={`${sensor.name}/${sensor.instance}`}>
                      <th scope="row">{sensor.name}</th>
                      <td>{sensor.instance || "–"}</td>
                      <td>{sensor.kind}</td>
                      <td>{formatSeconds(sensor.interval_ms / 1000)}</td>
                      <td>{sensor.timeout_ms === null ? "agent default" : formatSeconds(sensor.timeout_ms / 1000)}</td>
                      <td>
                        {Object.keys(sensor.params).length === 0
                          ? "–"
                          : Object.entries(sensor.params).map(([key, value]) => (
                              <code key={key} className={styles.param}>
                                {key}={value}
                              </code>
                            ))}
                      </td>
                      <td>{ORIGINS[sensor.origin]}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <p className={styles.note}>Editing comes later; site and server rows are managed through the API for now.</p>
        </>
      )}
    </section>
  );
}

/** Whether the agent acknowledged the current revision (ADR-0016). */
function Drift({ config, now }: { config: EffectiveSensors; now: Date }) {
  const applied = config.applied_at && (
    <time dateTime={config.applied_at} title={formatDateTime(config.applied_at)}>
      {formatRelative(config.applied_at, now)}
    </time>
  );
  if (config.applied_revision === null) {
    return (
      <p className={styles.pending}>
        Not applied: the agent has never acknowledged a configuration. The current revision is {config.revision}.
      </p>
    );
  }
  if (config.in_sync) {
    return (
      <p className={styles.inSync}>
        In sync: the agent runs revision {config.revision}
        {applied && <>, applied {applied}</>}.
      </p>
    );
  }
  return (
    <p className={styles.pending}>
      Out of date: the agent runs revision {config.applied_revision}
      {applied && <> (applied {applied})</>}, the current one is {config.revision}. A connected agent gets it within
      about 10 seconds.
    </p>
  );
}
