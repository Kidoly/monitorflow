"use client";

import { type ReactNode, useCallback, useMemo, useState } from "react";

import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";
import {
  type AvailabilityCheck,
  type AvailabilityMachine,
  type AvailabilityMaintenance,
  type AvailabilityOutage,
  type AvailabilityQuery,
  type AvailabilityReport,
  type AvailabilityService,
  api,
  availabilityFileHref,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { useMultiCustomer, useMultiSite } from "@/lib/me";
import { browserTimeZone } from "@/lib/onCall";
import { PERIOD_CHOICES, type PeriodChoice, dateInput, describeCause, formatAvailability, formatTarget, periodOf } from "@/lib/reports";
import { useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { buttonClass, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InfoTip } from "@/components/ui/Hint";
import { Page } from "@/components/ui/Page";
import { Banner, ErrorState, PageHeader } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatDuration, formatExact } from "@/components/ui/time";

import admin from "./AdminPages.module.css";
import styles from "./Reports.module.css";

/** Reports are not live: once every five minutes is enough. */
const REFRESH_MS = 5 * 60_000;

const when = (iso: string) => formatExact(new Date(iso)).slice(0, 16);

function Section({ title, children, note }: { title: string; children: ReactNode; note?: string }) {
  return (
    <section aria-label={title} className={styles.section}>
      <h2 className={styles.sectionTitle}>{title}</h2>
      {children}
      {note && <p className={admin.fieldNote}>{note}</p>}
    </section>
  );
}

function Tile({ label, value }: { label: string; value: ReactNode }) {
  return (
    <div className={styles.tile}>
      <span className={styles.tileLabel}>{label}</span>
      <span className={styles.tileValue}>{value}</span>
    </div>
  );
}

function Result({ service }: { service: AvailabilityService }) {
  if (service.met === null) return <Label tone="neutral">{t("reports.noTarget")}</Label>;
  return service.met ? <Label tone="ok">{t("reports.met")}</Label> : <Label tone="bad">{t("reports.missed")}</Label>;
}

function Budget({ service }: { service: AvailabilityService }) {
  const budget = service.budget_seconds;
  if (budget === null) return <>{t("reports.none")}</>;
  return budget >= 0 ? <>{formatDuration(budget)}</> : <span className={styles.bad}>{t("reports.exceeded", { time: formatDuration(-budget) })}</span>;
}

function Summary({ report }: { report: AvailabilityReport }) {
  const s = report.summary;
  const targets = s.services_met + s.services_missed;
  const sentence = [
    t("reports.summary.watched", { count: s.machines }),
    s.outages === 0 ? t("reports.summary.noOutage") : t("reports.summary.outageLine", { count: s.outages, down: formatDuration(s.down_seconds) }),
  ].join(" ");
  return (
    <Section title={t("reports.summary.title")}>
      <div className={styles.tiles}>
        <Tile label={t("reports.summary.outages")} value={s.outages} />
        <Tile label={t("reports.summary.down")} value={formatDuration(s.down_seconds)} />
        <Tile label={t("reports.summary.maintenance")} value={s.maintenance_seconds > 0 ? formatDuration(s.maintenance_seconds) : t("reports.none")} />
        {s.services > 0 && (
          <Tile
            label={t("reports.summary.services")}
            value={targets > 0 ? t("reports.summary.servicesValue", { met: s.services_met, total: targets }) : t("reports.summary.noTarget")}
          />
        )}
      </div>
      <p className={styles.sentence}>{sentence}</p>
    </Section>
  );
}

function Services({ services }: { services: AvailabilityService[] }) {
  const columns: Column<AvailabilityService>[] = [
    { key: "name", header: t("reports.column.service"), cell: (s) => s.name },
    { key: "availability", header: t("reports.column.availability"), cell: (s) => formatAvailability(s.numbers.availability) },
    { key: "target", header: t("reports.column.target"), cell: (s) => (s.target === null ? t("reports.none") : formatTarget(s.target)) },
    { key: "result", header: t("reports.column.result"), cell: (s) => <Result service={s} /> },
    { key: "outages", header: t("reports.column.outages"), cell: (s) => s.numbers.outages },
    { key: "down", header: t("reports.column.down"), cell: (s) => formatDuration(s.numbers.down_seconds) },
    { key: "budget", header: t("reports.column.budget"), cell: (s) => <Budget service={s} /> },
  ];
  return <DataTable label={t("reports.services.title")} columns={columns} rows={services} rowKey={(s) => s.id} />;
}

function Machines({ machines }: { machines: AvailabilityMachine[] }) {
  const multiSite = useMultiSite();
  const columns: Column<AvailabilityMachine>[] = [
    { key: "name", header: t("reports.column.machine"), cell: (m) => m.name },
    ...(multiSite ? [{ key: "site", header: t("reports.column.site"), cell: (m: AvailabilityMachine) => m.site_name }] : []),
    { key: "availability", header: t("reports.column.availability"), cell: (m) => formatAvailability(m.numbers.availability) },
    { key: "outages", header: t("reports.column.outages"), cell: (m) => m.numbers.outages },
    { key: "down", header: t("reports.column.down"), cell: (m) => formatDuration(m.numbers.down_seconds) },
    { key: "longest", header: t("reports.column.longest"), cell: (m) => (m.numbers.outages ? formatDuration(m.numbers.longest_seconds) : t("reports.none")) },
    {
      key: "maintenance",
      header: t("reports.column.maintenance"),
      cell: (m) => (m.numbers.maintenance_seconds > 0 ? formatDuration(m.numbers.maintenance_seconds) : t("reports.none")),
    },
  ];
  return <DataTable label={t("reports.machines.title")} columns={columns} rows={machines} rowKey={(m) => m.id} />;
}

function Checks({ checks }: { checks: AvailabilityCheck[] }) {
  const columns: Column<AvailabilityCheck>[] = [
    { key: "server", header: t("reports.column.machine"), cell: (c) => c.server_name },
    { key: "check", header: t("reports.column.check"), cell: (c) => (c.instance ? `${c.name} (${c.instance})` : c.name) },
    { key: "availability", header: t("reports.column.availability"), cell: (c) => formatAvailability(c.numbers.availability) },
    { key: "outages", header: t("reports.column.outages"), cell: (c) => c.numbers.outages },
    { key: "down", header: t("reports.column.down"), cell: (c) => formatDuration(c.numbers.down_seconds) },
  ];
  return <DataTable label={t("reports.checks.title")} columns={columns} rows={checks} rowKey={(c) => `${c.server_id}\t${c.name}`} />;
}

function Outages({ outages }: { outages: AvailabilityOutage[] }) {
  const columns: Column<AvailabilityOutage>[] = [
    { key: "start", header: t("reports.column.start"), cell: (o) => when(o.start) },
    { key: "end", header: t("reports.column.end"), cell: (o) => (o.ongoing ? <Label tone="bad">{t("reports.ongoing")}</Label> : when(o.end)) },
    { key: "length", header: t("reports.column.length"), cell: (o) => formatDuration(o.seconds) },
    { key: "what", header: t("reports.column.what"), cell: (o) => o.causes.map(describeCause).join("; ") },
  ];
  return <DataTable label={t("reports.outages.title")} columns={columns} rows={outages} rowKey={(o) => `${o.start}\t${o.causes[0]?.server_id}`} />;
}

function Maintenance({ rows }: { rows: AvailabilityMaintenance[] }) {
  const columns: Column<AvailabilityMaintenance>[] = [
    { key: "start", header: t("reports.column.start"), cell: (m) => when(m.start) },
    { key: "end", header: t("reports.column.end"), cell: (m) => when(m.end) },
    { key: "reason", header: t("reports.column.reason"), cell: (m) => m.reason },
    { key: "covered", header: t("reports.column.covered"), cell: (m) => m.servers.join(", ") },
  ];
  return <DataTable label={t("reports.maintenance.title")} columns={columns} rows={rows} rowKey={(m) => `${m.start}\t${m.reason}\t${m.servers.join()}`} />;
}

/** The report itself: summary first, then the details. */
export function ReportView({ report }: { report: AvailabilityReport }) {
  const outages = report.machines.flatMap((m) => m.outage_list).sort((a, b) => a.start.localeCompare(b.start));
  return (
    <>
      <Summary report={report} />
      {report.services.length > 0 && (
        <Section title={t("reports.services.title")}>
          <Services services={report.services} />
        </Section>
      )}
      <Section title={t("reports.machines.title")}>
        {report.machines.length ? <Machines machines={report.machines} /> : <p className={admin.fieldNote}>{t("reports.machines.empty")}</p>}
      </Section>
      {report.checks.length > 0 && (
        <Section title={t("reports.checks.title")} note={t("reports.checks.note")}>
          <Checks checks={report.checks} />
        </Section>
      )}
      {outages.length > 0 && (
        <Section title={t("reports.outages.title")}>
          <Outages outages={outages} />
        </Section>
      )}
      {report.maintenance.length > 0 && (
        <Section title={t("reports.maintenance.title")}>
          <Maintenance rows={report.maintenance} />
        </Section>
      )}
    </>
  );
}

/** Reports: availability over a period, on screen and as PDF and CSV (ADR-0155). */
export function ReportsPage({ children }: { children?: ReactNode }) {
  const scope = useScope();
  const multiCustomer = useMultiCustomer();
  const [choice, setChoice] = useState<PeriodChoice>("thisMonth");
  const [custom, setCustom] = useState(() => {
    const now = new Date();
    return { from: dateInput(new Date(now.getFullYear(), now.getMonth(), 1)), to: dateInput(now) };
  });
  // The period is fixed when chosen, so "this month" does not move under the reader.
  const [now] = useState(() => new Date());
  const period = useMemo(() => periodOf(choice, now, custom), [choice, now, custom]);
  const query: AvailabilityQuery | null = period && {
    from: period.from.toISOString(),
    to: period.to.toISOString(),
    customer_id: scope.customerId,
  };
  const key = query ? `${query.from}\t${query.to}\t${query.customer_id ?? ""}` : "";
  const fetcher = useCallback(
    (signal: AbortSignal) => {
      const [from = "", to = "", owner = ""] = key.split("\t");
      return api.availability({ from, to, customer_id: owner || null }, signal);
    },
    [key],
  );
  const { data, error, refresh } = usePolledData(fetcher, { intervalMs: REFRESH_MS, enabled: query !== null });
  const report = data && query && data.from === query.from ? data : null;
  const zone = browserTimeZone();
  const language = activeLocale();
  return (
    <Page>
      <PageHeader title={t("reports.title")} status={<InfoTip label={t("reports.about")}>{t("reports.rule")}</InfoTip>} />
      <div className={styles.controls}>
        <label className={admin.field}>
          {t("reports.period")}
          <select className={fieldClass} value={choice} onChange={(e) => setChoice(e.target.value as PeriodChoice)}>
            {PERIOD_CHOICES.map((c) => (
              <option key={c} value={c}>
                {t(`reports.period.${c}`)}
              </option>
            ))}
          </select>
        </label>
        {choice === "custom" && (
          <>
            <label className={admin.field}>
              {t("reports.period.from")}
              <input className={fieldClass} type="date" value={custom.from} onChange={(e) => setCustom({ ...custom, from: e.target.value })} />
            </label>
            <label className={admin.field}>
              {t("reports.period.to")}
              <input className={fieldClass} type="date" value={custom.to} onChange={(e) => setCustom({ ...custom, to: e.target.value })} />
            </label>
          </>
        )}
        {query && (
          <div className={styles.downloads}>
            <a className={buttonClass({ variant: "k" })} href={availabilityFileHref(query, "pdf", language, zone)} download>
              {t("reports.downloadPdf")}
            </a>
            <a className={buttonClass()} href={availabilityFileHref(query, "csv", language, zone)} download>
              {t("reports.downloadCsv")}
            </a>
          </div>
        )}
      </div>
      {period ? (
        <p className={admin.fieldNote}>
          {t("reports.period.range", { from: when(period.from.toISOString()), to: when(period.to.toISOString()) })}
          {multiCustomer && report?.customer_name ? ` · ${t("reports.forCustomer", { name: report.customer_name })}` : ""}
        </p>
      ) : (
        <Banner tone="warn">{t("reports.period.invalid")}</Banner>
      )}
      {query && !report && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("reports.load")} error={describeError(error)} onRetry={refresh} />
      ) : report ? (
        <ReportView report={report} />
      ) : null}
      {children}
    </Page>
  );
}
