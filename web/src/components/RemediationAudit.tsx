"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback, useEffect, useMemo, useState } from "react";

import {
  type RemediationEvent,
  type RemediationIncidentDetail,
  type Server,
  api,
  describeError,
  isAbortError,
  isUnauthorized,
} from "@/lib/api";
import {
  ACTOR_KINDS,
  type AuditFilters,
  EVENT_KINDS,
  actorLabel,
  auditFiltersFromQuery,
  auditQuery,
  chainSummary,
  eventKindLabel,
  eventSummary,
} from "@/lib/remediation";
import { APPROVALS_PATH, auditHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { seriesTitle } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar } from "@/components/ui/FilterBar";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { formatDay, formatExact } from "@/components/ui/time";

import styles from "./Remediation.module.css";

/** Events per "Load more". */
export const PAGE_SIZE = 50;
/** The API's largest page, which caps how far "Load more" goes. */
const MAX_LIMIT = 500;

/** The audit log for the page's query string, which holds its filters. */
export function RemediationAuditView() {
  const search = useSearchParams().toString();
  const filters = useMemo(() => auditFiltersFromQuery(new URLSearchParams(search)), [search]);
  // New filters start from the first page instead of extending the old list.
  return <RemediationAudit key={search} filters={filters} />;
}

interface Loaded {
  events: RemediationEvent[];
  more: boolean;
  /** The filtered incident with its chain report; null when unreadable. */
  incident: RemediationIncidentDetail | null;
}

/**
 * The remediation audit log (ADR-0045): what the AI, the rules, approvers
 * and agents did, across incidents, newest first. Filtered to one incident,
 * it shows whether that incident's hash chain verifies.
 */
export function RemediationAudit({ filters }: { filters: AuditFilters }) {
  const scope = useScope();
  const [limit, setLimit] = useState(PAGE_SIZE);
  const [servers, setServers] = useState<Server[]>([]);

  useEffect(() => {
    const controller = new AbortController();
    api.servers(controller.signal).then((r) => setServers(r.servers), () => {});
    return () => controller.abort();
  }, []);

  const fetcher = useCallback(
    async (signal: AbortSignal): Promise<Loaded> => {
      const query = {
        ...auditQuery(filters),
        customer_id: scope.customerId ?? undefined,
        site_id: scope.siteId ?? undefined,
        limit,
      };
      const [page, incident] = await Promise.all([
        api.remediationEvents(query, signal),
        filters.incident
          ? api.remediationIncident(filters.incident, signal).catch((e: unknown) => {
              if (isAbortError(e)) throw e;
              return null;
            })
          : null,
      ]);
      return { events: page.events, more: page.next_cursor !== null, incident };
    },
    [filters, limit, scope.customerId, scope.siteId],
  );
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const now = updatedAt ?? new Date();
  const filtered = Boolean(filters.incident || filters.server || filters.kind || filters.actor);

  const serverChoices = servers
    .filter((s) => (!scope.customerId || s.customer_id === scope.customerId) && (!scope.siteId || s.site_id === scope.siteId))
    .sort((a, b) => a.hostname.localeCompare(b.hostname, "en", { numeric: true }));

  const columns: Column<RemediationEvent>[] = [
    {
      key: "time",
      header: "Time",
      cell: (e) => (
        <time dateTime={e.created_at} title={formatExact(new Date(e.created_at))} className={styles.dim}>
          {formatDay(new Date(e.created_at), now)}
        </time>
      ),
    },
    { key: "actor", header: "Actor", cell: (e) => <strong>{actorLabel(e)}</strong> },
    { key: "event", header: "Event", cell: (e) => eventKindLabel(e.kind) },
    {
      key: "target",
      header: "Target",
      cell: (e) => (
        <PrimaryCell
          title={e.hostname}
          href={scopedHref(serverHref(e.server_id), scope)}
          sub={
            <>
              {e.site_name} ·{" "}
              <Link href={scopedHref(auditHref({ incident: e.incident_id }), scope)} title="Only this incident">
                {seriesTitle(undefined, e.metric, e.instance)}, event {e.seq}
              </Link>
            </>
          }
        />
      ),
    },
    { key: "details", header: "Details", cell: (e) => eventSummary(e), className: styles.wrap },
  ];

  const incident = data?.incident;
  return (
    <Page>
      <PageHeader
        title="Audit log"
        meta={
          <>
            Remediation history, append-only · <RefreshLine updatedAt={updatedAt} stale={stale} />
          </>
        }
        actions={<ButtonLink href={scopedHref(APPROVALS_PATH, scope)}>Approvals</ButtonLink>}
      />
      <FilterBar
        filters={[
          { name: "kind", label: "Event", options: EVENT_KINDS },
          { name: "actor", label: "Actor", options: ACTOR_KINDS },
          { name: "server", label: "Server", options: serverChoices.map((s) => ({ value: s.id, label: s.hostname })) },
        ]}
      />
      {filters.incident ? (
        incident ? (
          <Banner tone={incident.chain.valid ? "ok" : "bad"}>
            <strong>
              {seriesTitle(undefined, incident.metric, incident.instance)} on {incident.hostname}
            </strong>{" "}
            · {chainSummary(incident.chain)}{" "}
            <Link href={scopedHref(auditHref(), scope)}>Show every incident</Link>
          </Banner>
        ) : (
          data && (
            <Banner tone="warn">
              This incident&apos;s chain could not be verified.{" "}
              <Link href={scopedHref(auditHref(), scope)}>Show every incident</Link>
            </Banner>
          )
        )
      ) : (
        <Banner tone="plain">
          Each incident&apos;s history is hash-chained. Choose an incident in the Target column to verify its chain.
        </Banner>
      )}
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what="the audit log" error={describeError(error)} onRetry={refresh} />
      ) : data && data.events.length === 0 ? (
        <EmptyState>{filtered ? "No events match these filters." : "No remediation has run yet."}</EmptyState>
      ) : (
        <div className={styles.page}>
          <DataTable
            label="Remediation events"
            columns={columns}
            rows={data?.events ?? []}
            rowKey={(e) => `${e.incident_id}:${e.seq}`}
            loading={!data}
            hasMore={Boolean(data?.more) && limit < MAX_LIMIT}
            onLoadMore={() => setLimit((n) => Math.min(MAX_LIMIT, n + PAGE_SIZE))}
            summary={(shown) => `Showing ${shown} ${shown === 1 ? "event" : "events"} · ${PAGE_SIZE} per page`}
          />
        </div>
      )}
    </Page>
  );
}
