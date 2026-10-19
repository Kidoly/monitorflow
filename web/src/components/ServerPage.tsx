"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type ReactNode, useCallback, useEffect, useState } from "react";

import { ApiError, type EffectiveThreshold, type Server, api, describeError, isAbortError } from "@/lib/api";
import { formatDateTime, formatOs, formatRelative } from "@/lib/format";
import { thresholdLines } from "@/lib/thresholds";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";

import { REFRESH_MS, StatusBadge } from "./FleetOverview";
import { SensorConfigPanel } from "./SensorConfigPanel";
import { type ChartPlan, ServerGraphs } from "./ServerGraphs";
import styles from "./ServerPage.module.css";

/** The server page for the `id` query parameter. */
export function ServerView() {
  const id = useSearchParams().get("id");
  // A new ID starts from a clean state instead of showing the old server.
  return <ServerPage key={id} id={id} />;
}

interface State {
  /** Undefined until loaded, null when the server is not in the list. */
  server: Server | null | undefined;
  updatedAt: Date | null;
  error: string | null;
  /** Effective thresholds, for the graphs' reference lines; the last ones received. */
  thresholds: EffectiveThreshold[];
  thresholdsError: string | null;
}

export function ServerPage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const [state, setState] = useState<State>({
    server: undefined,
    updatedAt: null,
    error: null,
    thresholds: [],
    thresholdsError: null,
  });

  // There is no single-server endpoint yet, so the header comes from the list.
  // Thresholds load in the same cycle; without them the graphs still show.
  const load = useCallback(
    async (signal: AbortSignal) => {
      const thresholds = api.serverThresholds(id ?? "", signal).then(
        (t) => ({ thresholds: t.thresholds, thresholdsError: null }),
        (e: unknown) => {
          if (isAbortError(e)) throw e;
          return { thresholdsError: describeError(e) };
        },
      );
      try {
        const [{ servers }, lines] = await Promise.all([api.servers(signal), thresholds]);
        const server = servers.find((s) => s.id === id?.toLowerCase()) ?? null;
        setState((s) => ({ ...s, ...lines, server, updatedAt: new Date(), error: null }));
      } catch (e) {
        // A 401 has already been routed to the login page.
        if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) {
          setState((s) => ({ ...s, error: describeError(e) }));
        }
      }
    },
    [id],
  );
  usePolling(load, REFRESH_MS, valid);

  const { server, updatedAt, error, thresholds, thresholdsError } = state;
  const referenceLines = useCallback(
    (plan: ChartPlan, instances: string[]) => thresholdLines(thresholds, plan.metric, instances),
    [thresholds],
  );
  useEffect(() => {
    if (server) document.title = `${server.hostname} · MonitorFlow`;
  }, [server]);

  if (!valid || server === null) {
    return (
      <section className={styles.notFound}>
        <h1>Server not found</h1>
        <p>It may have been removed, or your roles do not cover its site.</p>
        <p>
          <Link href="/">Back to the fleet</Link>
        </p>
      </section>
    );
  }
  if (!server || !updatedAt) {
    return error ? (
      <p className={styles.error} role="alert">
        {error}
      </p>
    ) : (
      <p role="status">Loading server…</p>
    );
  }
  return (
    <article aria-labelledby="server-title">
      <ServerHeader server={server} now={updatedAt} />
      {error && (
        <p className={styles.error} role="alert">
          Refresh failed: {error} Showing the last data received.
        </p>
      )}
      {thresholdsError && (
        <p className={styles.note}>Threshold lines are not shown: {thresholdsError}</p>
      )}
      <ServerGraphs serverId={server.id} referenceLines={referenceLines} />
      <SensorConfigPanel serverId={server.id} />
    </article>
  );
}

function ServerHeader({ server, now }: { server: Server; now: Date }) {
  return (
    <header className={styles.header}>
      <p className={styles.crumbs}>
        <Link href="/">Fleet</Link> / {server.site_name}
      </p>
      <div className={styles.title}>
        <h1 id="server-title">{server.hostname}</h1>
        <StatusBadge server={server} />
      </div>
      <dl className={styles.facts}>
        <Fact term="Site">{server.site_name}</Fact>
        <Fact term="OS">{formatOs(server.os_family, server.os_version)}</Fact>
        <Fact term="Agent">{server.agent_version ?? "unknown"}</Fact>
        <Fact term="Last seen">
          {server.last_heartbeat ? <Time iso={server.last_heartbeat} now={now} /> : "never"}
        </Fact>
        {server.connected_at && (
          <Fact term="Connected">
            <Time iso={server.connected_at} now={now} />
          </Fact>
        )}
        <Fact term="Certificate ID">
          <code>{server.peer_id}</code>
        </Fact>
        <Fact term="Enrolled">{formatDateTime(server.created_at)}</Fact>
        {server.revoked_at && <Fact term="Revoked">{formatDateTime(server.revoked_at)}</Fact>}
      </dl>
    </header>
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

function Time({ iso, now }: { iso: string; now: Date }) {
  return (
    <time dateTime={iso} title={formatDateTime(iso)}>
      {formatRelative(iso, now)}
    </time>
  );
}
