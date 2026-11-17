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
  type AuditFilters,
  actorKindOptions,
  actorLabel,
  auditFiltersFromQuery,
  auditQuery,
  chainSummary,
  eventKindLabel,
  eventKindOptions,
  eventSummary,
} from "@/lib/remediation";
import { t } from "@/i18n";
import { auditHref, incidentsHref, serverHref } from "@/lib/routes";
import { useMultiSite } from "@/lib/me";
import { scopedHref, useScope } from "@/lib/scope";
import { seriesTitle } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { FilterBar, NoMatches } from "@/components/ui/FilterBar";
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
  const severalSites = useMultiSite();
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
      header: t("remediation.audit.time"),
      cell: (e) => (
        <time dateTime={e.created_at} title={formatExact(new Date(e.created_at))} className={styles.dim}>
          {formatDay(new Date(e.created_at), now)}
        </time>
      ),
    },
    { key: "actor", header: t("remediation.audit.actor"), cell: (e) => <strong>{actorLabel(e)}</strong> },
    { key: "event", header: t("remediation.audit.event"), cell: (e) => eventKindLabel(e.kind) },
    {
      key: "target",
      header: t("remediation.audit.target"),
      cell: (e) => (
        <PrimaryCell
          title={e.hostname}
          href={scopedHref(serverHref(e.server_id), scope)}
          sub={
            <>
              {severalSites && `${e.site_name} · `}
              <Link href={scopedHref(auditHref({ incident: e.incident_id }), scope)} title={t("remediation.audit.onlyThis")}>
                {t("remediation.audit.eventOf", { title: seriesTitle(undefined, e.metric, e.instance), seq: String(e.seq) })}
              </Link>
            </>
          }
        />
      ),
    },
    { key: "details", header: t("remediation.audit.details"), cell: (e) => eventSummary(e), className: styles.wrap },
  ];

  const incident = data?.incident;
  return (
    <Page>
      <PageHeader
        title={t("remediation.audit.title")}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
      />
      <FilterBar
        filters={[
          { name: "kind", label: t("remediation.audit.event"), options: eventKindOptions() },
          { name: "actor", label: t("remediation.audit.actor"), options: actorKindOptions() },
          { name: "server", label: t("remediation.audit.server"), options: serverChoices.map((s) => ({ value: s.id, label: s.hostname })) },
        ]}
      />
      {filters.incident ? (
        incident ? (
          <Banner tone={incident.chain.valid ? "ok" : "bad"}>
            <strong>
              {t("remediation.audit.on", {
                title: seriesTitle(undefined, incident.metric, incident.instance),
                hostname: incident.hostname,
              })}
            </strong>{" "}
            · {chainSummary(incident.chain)}{" "}
            <Link href={scopedHref(auditHref(), scope)}>{t("remediation.audit.showAll")}</Link>
          </Banner>
        ) : (
          data && (
            <Banner tone="warn">
              {t("remediation.audit.unverified")}{" "}
              <Link href={scopedHref(auditHref(), scope)}>{t("remediation.audit.showAll")}</Link>
            </Banner>
          )
        )
      ) : (
        <Banner tone="plain">
          {t("remediation.audit.chained")}
        </Banner>
      )}
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("remediation.audit.what")} error={describeError(error)} onRetry={refresh} />
      ) : data && data.events.length === 0 ? (
        filtered ? (
          <NoMatches names={["incident", "server", "kind", "actor"]}>{t("remediation.audit.noMatch")}</NoMatches>
        ) : (
          <EmptyState
            actions={<ButtonLink href={scopedHref(incidentsHref(), scope)}>{t("remediation.audit.openIncidents")}</ButtonLink>}
          >
            {t("remediation.audit.none")}
          </EmptyState>
        )
      ) : (
        <div className={styles.page}>
          <DataTable
            label={t("remediation.audit.label")}
            columns={columns}
            rows={data?.events ?? []}
            rowKey={(e) => `${e.incident_id}:${e.seq}`}
            loading={!data}
            hasMore={Boolean(data?.more) && limit < MAX_LIMIT}
            onLoadMore={() => setLimit((n) => Math.min(MAX_LIMIT, n + PAGE_SIZE))}
            summary={(shown) => t("remediation.audit.showing", { count: shown, size: PAGE_SIZE })}
          />
        </div>
      )}
    </Page>
  );
}
