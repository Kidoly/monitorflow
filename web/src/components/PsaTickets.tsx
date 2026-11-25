"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  type PsaConnection,
  type PsaRecord,
  type PsaTicket,
  type PsaTicketSettingsRequest,
  api,
  isAbortError,
} from "@/lib/api";
import { incidentTitle } from "@/lib/incidents";
import { incidentHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { psaError } from "./PsaSiteLinks";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { EmptyState, ErrorState, LoadingBlock } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

const TICKET_TONES: Record<PsaTicket["state"], Tone> = {
  creating: "info",
  open: "warn",
  closed: "ok",
  skipped: "neutral",
  failed: "bad",
  held: "info",
  ended: "neutral",
};

interface Lists {
  types: PsaRecord[];
  categories: string[];
  statuses: PsaRecord[];
}

/** The tickets step of the HaloPSA page: what tickets look like, then the latest ones (ADR-0166, ADR-0191). */
export function PsaTickets({ connection, onSaved }: { connection: PsaConnection; onSaved: () => void }) {
  return (
    <section className={styles.section} aria-label={t("psa.step.tickets")}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("psa.step.tickets")}</h2>
      </div>
      <p className={styles.sectionNote}>{t("psa.tickets.intro")}</p>
      <div className={styles.sectionBody}>
        <TicketSettings connection={connection} onSaved={onSaved} />
        <RecentTickets />
      </div>
    </section>
  );
}

function TicketSettings({ connection, onSaved }: { connection: PsaConnection; onSaved: () => void }) {
  const saved = connection.tickets;
  const [lists, setLists] = useState<Lists | { error: string } | null>(null);
  const [type, setType] = useState(saved.ticket_type_id === null ? "" : String(saved.ticket_type_id));
  const [category, setCategory] = useState(saved.category ?? "");
  const [severity, setSeverity] = useState(saved.min_severity);
  const [status, setStatus] = useState(saved.resolved_status_id === null ? "" : String(saved.resolved_status_id));
  const [warningPriority, setWarningPriority] = useState(
    saved.warning_priority_id === null ? "" : String(saved.warning_priority_id),
  );
  const [criticalPriority, setCriticalPriority] = useState(
    saved.critical_priority_id === null ? "" : String(saved.critical_priority_id),
  );
  const [closed, setClosed] = useState<number[]>(saved.closed_status_ids);
  const [onAssign, setOnAssign] = useState(saved.acknowledge_on_assign);
  // Keyed by the ticket type they were read for.
  const [read, setRead] = useState<{ type: string; records: PsaRecord[] } | null>(null);
  const priorities = read?.type === type ? read.records : null;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      api.psaTicketTypes(controller.signal),
      api.psaCategories(controller.signal),
      api.psaStatuses(controller.signal),
    ])
      .then(([types, categories, statuses]) =>
        setLists({ types: types.records, categories: categories.categories, statuses: statuses.records }),
      )
      .catch((e: unknown) => {
        if (!isAbortError(e)) setLists({ error: psaError(e) });
      });
    return () => controller.abort();
  }, []);

  // The priorities belong to the ticket type's SLA: read again when it changes.
  useEffect(() => {
    const controller = new AbortController();
    api
      .psaPriorities(type ? Number(type) : null, controller.signal)
      .then((list) => setRead({ type, records: list.records }))
      .catch((e: unknown) => {
        if (!isAbortError(e)) setRead({ type, records: [] });
      });
    return () => controller.abort();
  }, [type]);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const body: PsaTicketSettingsRequest = {
      ticket_type_id: type ? Number(type) : null,
      category: category || null,
      min_severity: severity,
      resolved_status_id: status ? Number(status) : null,
      warning_priority_id: warningPriority ? Number(warningPriority) : null,
      critical_priority_id: criticalPriority ? Number(criticalPriority) : null,
      closed_status_ids: closed,
      acknowledge_on_assign: onAssign,
    };
    setBusy(true);
    setError(null);
    try {
      await api.savePsaTicketSettings(body);
      setDone(true);
      onSaved();
    } catch (e) {
      setError(psaError(e));
    } finally {
      setBusy(false);
    }
  }

  if (lists === null) return <span className={styles.dim}>{t("psa.tickets.reading")}</span>;
  if ("error" in lists) {
    return (
      <p className={styles.formError} role="alert">
        {lists.error}
      </p>
    );
  }
  const changed = () => setDone(false);
  return (
    <form className={styles.stack} onSubmit={submit} aria-label={t("psa.step.tickets")}>
      <label className={styles.field}>
        {t("psa.tickets.type")}
        <select
          className={fieldClass}
          value={type}
          onChange={(e) => {
            setType(e.target.value);
            setWarningPriority("");
            setCriticalPriority("");
            changed();
          }}
        >
          <option value="">{t("psa.tickets.typeDefault")}</option>
          {lists.types.map((r) => (
            <option key={r.id} value={String(r.id)}>
              {r.name}
            </option>
          ))}
        </select>
      </label>
      <label className={styles.field}>
        {t("psa.tickets.category")}
        <select
          className={fieldClass}
          value={category}
          onChange={(e) => {
            setCategory(e.target.value);
            changed();
          }}
        >
          <option value="">{t("psa.tickets.categoryNone")}</option>
          {lists.categories.map((c) => (
            <option key={c} value={c}>
              {c}
            </option>
          ))}
        </select>
        <span className={styles.fieldNote}>{t("psa.tickets.categoryNote")}</span>
      </label>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("psa.tickets.severity")}</legend>
        <div className={styles.radios}>
          {(["critical", "warning"] as const).map((s) => (
            <label key={s}>
              <input
                type="radio"
                name="min-severity"
                checked={severity === s}
                onChange={() => {
                  setSeverity(s);
                  changed();
                }}
              />{" "}
              {t(`psa.tickets.${s}`)}
            </label>
          ))}
        </div>
      </fieldset>
      <label className={styles.field}>
        {t("psa.tickets.resolved")}
        <select
          className={fieldClass}
          value={status}
          onChange={(e) => {
            setStatus(e.target.value);
            changed();
          }}
        >
          <option value="">{t("psa.tickets.noteOnly")}</option>
          {lists.statuses.map((r) => (
            <option key={r.id} value={String(r.id)}>
              {t("psa.tickets.moveTo", { status: r.name })}
            </option>
          ))}
        </select>
      </label>
      {(["warning", "critical"] as const).map((level) => {
        const value = level === "warning" ? warningPriority : criticalPriority;
        const set = level === "warning" ? setWarningPriority : setCriticalPriority;
        return (
          <label key={level} className={styles.field}>
            {t(`psa.tickets.priority.${level}`)}
            <select
              className={fieldClass}
              value={value}
              disabled={priorities === null}
              onChange={(e) => {
                set(e.target.value);
                changed();
              }}
            >
              <option value="">{priorities === null ? t("psa.tickets.priorityReading") : t("psa.tickets.priorityDefault")}</option>
              {priorities?.map((r) => (
                <option key={r.id} value={String(r.id)}>
                  {r.name}
                </option>
              ))}
            </select>
            {level === "critical" && <span className={styles.fieldNote}>{t("psa.tickets.priorityNote")}</span>}
          </label>
        );
      })}
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("psa.tickets.closedStatuses")}</legend>
        <div className={styles.radios}>
          {lists.statuses.map((r) => (
            <label key={r.id}>
              <input
                type="checkbox"
                checked={closed.includes(r.id)}
                onChange={(e) => {
                  setClosed(e.target.checked ? [...closed, r.id] : closed.filter((id) => id !== r.id));
                  changed();
                }}
              />{" "}
              {r.name}
            </label>
          ))}
        </div>
        <span className={styles.fieldNote}>{t("psa.tickets.closedNote")}</span>
      </fieldset>
      <label>
        <input
          type="checkbox"
          checked={onAssign}
          onChange={(e) => {
            setOnAssign(e.target.checked);
            changed();
          }}
        />{" "}
        {t("psa.tickets.onAssign")}
      </label>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {t("psa.tickets.save")}
        </Button>
        {done && <span className={styles.dim}>{t("psa.tickets.saved")}</span>}
      </div>
    </form>
  );
}

function RecentTickets() {
  const fetcher = useCallback((signal: AbortSignal) => api.psaTickets(signal), []);
  const { data, error, refresh } = usePolledData(fetcher);
  if (!data) {
    return error ? (
      <ErrorState what={t("psa.tickets.what")} error={psaError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock />
    );
  }
  if (data.tickets.length === 0) return <EmptyState>{t("psa.tickets.none")}</EmptyState>;
  const now = new Date();
  const columns: Column<PsaTicket>[] = [
    {
      key: "incident",
      header: t("psa.column.incident"),
      cell: (row) => (
        <Link href={incidentHref(row.incident_id)}>
          {t("psa.ticket.incidentLink", {
            metric: incidentTitle({ metric: row.metric, instance: "" }),
            server: row.server_name ?? row.site_name ?? "",
          })}
        </Link>
      ),
    },
    {
      key: "ticket",
      header: t("psa.column.ticket"),
      cell: (row) => (row.ticket_id === null ? "" : t("psa.ticket.number", { id: String(row.ticket_id) })),
    },
    {
      key: "state",
      header: t("psa.column.state"),
      cell: (row) => (
        <span>
          <Label tone={TICKET_TONES[row.state]}>{t(`psa.ticket.${row.state}`)}</Label>
          {row.error && <span className={styles.fieldNote}> {row.error}</span>}
        </span>
      ),
    },
    {
      key: "when",
      header: t("psa.column.when"),
      cell: (row) => <RelativeTime value={row.updated_at} now={now} />,
    },
  ];
  return (
    <DataTable
      dense
      label={t("psa.tickets.recent")}
      columns={columns}
      rows={data.tickets}
      rowKey={(row) => row.incident_id}
    />
  );
}
