"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { useCallback } from "react";

import { EmptyState, ErrorState } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatSince } from "@/components/ui/time";
import { t } from "@/i18n";
import { type Incident, api, describeError } from "@/lib/api";
import { incidentTitle } from "@/lib/incidents";
import { mobileIncidentHref } from "@/lib/mobile";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import styles from "./Mobile.module.css";

/** One incident as a card: what, where, since when, and who is on it. */
export function IncidentCard({ incident, now }: { incident: Incident; now: Date }) {
  const open = incident.state === "open";
  return (
    <Link href={mobileIncidentHref(incident.id)} className={styles.card}>
      <span className={styles.cardTop}>
        <Label tone={!open ? "ok" : incident.severity === "critical" ? "bad" : "warn"} small>
          {open ? t(`sensors.severity.${incident.severity}`) : t("mobile.resolved")}
        </Label>
        <span className={styles.cardTitle}>{incidentTitle(incident)}</span>
      </span>
      <span>
        {incident.hostname} · {incident.site_name}
      </span>
      <span className={styles.muted}>
        {t("mobile.since", { time: formatSince(new Date(incident.opened_at), now) })}
        {incident.acknowledged_by ? ` · ${t("mobile.acknowledgedBy", { name: incident.acknowledged_by })}` : ""}
        {open && incident.silenced_until ? ` · ${t("mobile.silenced")}` : ""}
      </span>
    </Link>
  );
}

/**
 * The open alerts, newest first after the ones nobody is on; or what one
 * notification sent together (`?group=`), resolved or not.
 */
export function MobileIncidents() {
  const params = useSearchParams();
  const raw = params.get("group");
  const group = raw && UUID.test(raw) ? raw : null;
  const fetcher = useCallback(
    (signal: AbortSignal) => api.incidents(group ? { group } : { state: "open" }, signal),
    [group],
  );
  const { data, error } = usePolledData(fetcher);
  const now = new Date();

  const incidents = [...(data?.incidents ?? [])].sort(
    (a, b) => Number(Boolean(a.acknowledged_at)) - Number(Boolean(b.acknowledged_at)) || b.opened_at.localeCompare(a.opened_at),
  );
  return (
    <>
      <h1 className={styles.title}>{group ? t("mobile.groupTitle") : t("mobile.alertsTitle")}</h1>
      {error !== null && !data && <ErrorState what={t("mobile.alertsWhat")} error={describeError(error)} />}
      {data && incidents.length === 0 && <EmptyState title={t("mobile.noAlerts")}>{t("mobile.noAlertsHint")}</EmptyState>}
      <ul className={styles.list}>
        {incidents.map((incident) => (
          <li key={incident.id}>
            <IncidentCard incident={incident} now={now} />
          </li>
        ))}
      </ul>
    </>
  );
}
