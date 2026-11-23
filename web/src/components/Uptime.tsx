"use client";

import { type ReactNode, useCallback } from "react";

import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";
import {
  type AvailabilityNumbers,
  type AvailabilityReport,
  type AvailabilityService,
  type Uptime,
  type UptimeDay,
  type UptimeRow,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { browserTimeZone } from "@/lib/onCall";
import { formatAvailability, formatTarget } from "@/lib/reports";
import { usePolledData } from "@/lib/usePolledData";
import { cx } from "@/components/ui/cx";
import { ErrorState } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatDuration } from "@/components/ui/time";

import admin from "./AdminPages.module.css";
import styles from "./Uptime.module.css";

/** Uptime moves slowly: once every five minutes is enough. */
const REFRESH_MS = 5 * 60_000;

/** How a day looks: all of it up, a little less, much less, or not measured. */
export type Tone = "up" | "minor" | "major" | "none";

/** The color of a day (ADR-0172): any down time leaves "up"; under 99 % is "major". */
export function toneOf(day: Pick<UptimeDay, "availability" | "down_seconds">): Tone {
  if (day.availability === null) return "none";
  if (day.down_seconds <= 0) return "up";
  return day.availability >= 99 ? "minor" : "major";
}

/** The color of a window's percentage, by the same thresholds as days. */
function windowTone(value: number | null): Tone {
  if (value === null) return "none";
  if (value >= 100) return "up";
  return value >= 99 ? "minor" : "major";
}

const WINDOWS = ["day", "week", "month", "quarter"] as const;

/** "Sun 22 Nov", in the reader's language; the date is already local. */
function dayName(date: string): string {
  return new Date(`${date}T12:00:00Z`).toLocaleDateString(activeLocale(), {
    weekday: "short",
    day: "numeric",
    month: "short",
    timeZone: "UTC",
  });
}

function barLabel(day: UptimeDay): string {
  const values = { date: dayName(day.date), availability: formatAvailability(day.availability) };
  return day.down_seconds > 0
    ? t("reports.uptime.barDown", { ...values, down: formatDuration(day.down_seconds) })
    : t("reports.uptime.bar", values);
}

function rowName(row: UptimeRow): string {
  return row.check ? `${row.name} · ${row.check}` : row.name;
}

/** Whether a business service met its target over the period. */
export function Result({ service }: { service: AvailabilityService }) {
  if (service.met === null) return <Label tone="neutral">{t("reports.noTarget")}</Label>;
  return service.met ? <Label tone="ok">{t("reports.met")}</Label> : <Label tone="bad">{t("reports.missed")}</Label>;
}

/** The down time a service's target still allows, or by how much it was exceeded. */
export function Budget({ service }: { service: AvailabilityService }) {
  const budget = service.budget_seconds;
  if (budget === null) return <>{t("reports.none")}</>;
  return budget >= 0 ? <>{formatDuration(budget)}</> : <span className={styles.bad}>{t("reports.exceeded", { time: formatDuration(-budget) })}</span>;
}

/** What the report of the chosen period says of one row. */
type PeriodNumbers = { numbers: AvailabilityNumbers; service?: AvailabilityService; machine: boolean };

/** The key a row and the report's entry for the same thing share. */
function keyOf(kind: UptimeRow["kind"], id: string, check: string | null): string {
  return kind === "check" ? `check\t${id}\t${check ?? ""}` : kind === "service" ? `service\t${id}` : `machine\t${id}`;
}

/** The report's numbers of each service, server, device and check, by row key. */
function periodNumbers(report: AvailabilityReport): Map<string, PeriodNumbers> {
  const found = new Map<string, PeriodNumbers>();
  for (const s of report.services) found.set(keyOf("service", s.id, null), { numbers: s.numbers, service: s, machine: false });
  for (const m of report.machines) found.set(keyOf("server", m.id, null), { numbers: m.numbers, machine: true });
  // A check row of uptime is the check's name over all its instances.
  for (const c of report.checks) if (!c.instance) found.set(keyOf("check", c.server_id, c.name), { numbers: c.numbers, machine: false });
  return found;
}

/**
 * Which entries of `report` the uptime rows already show, so the report's
 * own tables list only the rest; `null` while uptime is not known.
 */
export function shownInUptime(uptime: Uptime | null): ((kind: "service" | "machine" | "check", id: string, check?: string) => boolean) | null {
  if (!uptime) return null;
  const keys = new Set(uptime.rows.map((r) => keyOf(r.kind, r.id, r.check)));
  return (kind, id, check) => keys.has(keyOf(kind === "machine" ? "server" : kind, id, check ?? null));
}

/** The chosen period's numbers under a row, with the same words as the report's tables. */
function PeriodLine({ name, found }: { name: string; found: PeriodNumbers }) {
  const n = found.numbers;
  const items: [string, ReactNode][] = [
    [name, formatAvailability(n.availability)],
    [t("reports.column.outages"), n.outages],
    [t("reports.column.down"), formatDuration(n.down_seconds)],
  ];
  if (found.machine) {
    items.push([t("reports.column.longest"), n.outages ? formatDuration(n.longest_seconds) : t("reports.none")]);
    items.push([t("reports.column.maintenance"), n.maintenance_seconds > 0 ? formatDuration(n.maintenance_seconds) : t("reports.none")]);
  }
  const service = found.service;
  if (service) {
    items.push([t("reports.column.target"), service.target === null ? t("reports.none") : formatTarget(service.target)]);
    items.push([t("reports.column.result"), <Result key="result" service={service} />]);
    items.push([t("reports.column.budget"), <Budget key="budget" service={service} />]);
  }
  return (
    <dl className={styles.period} aria-label={t("reports.uptime.period", { period: name })}>
      {items.map(([label, value], i) => (
        <div key={label} className={cx(styles.periodItem, i === 0 && styles.periodMain)}>
          <dt>{label}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}

/** One row: what it is, its four windows, its 90 days as bars, and the chosen period. */
function Row({ row, period }: { row: UptimeRow; period: { name: string; found: PeriodNumbers | undefined } | null }) {
  const name = rowName(row);
  return (
    <li className={cx(styles.row, row.kind === "check" && styles.check)}>
      <div className={styles.head}>
        <div className={styles.what}>
          <strong className={styles.name}>{name}</strong>
          <span className={admin.dim}>
            {[t(`reports.uptime.kind.${row.kind}`), row.site_name].filter(Boolean).join(" · ")}
            {" · "}
            {row.outages ? t("reports.uptime.outages", { count: row.outages }) : t("reports.uptime.noOutage")}
          </span>
        </div>
        <dl className={styles.windows}>
          {WINDOWS.map((w) => {
            const value = row.windows[w];
            return (
              <div key={w} className={styles.window}>
                <dt>{t(`reports.uptime.${w}`)}</dt>
                <dd className={cx(styles.percent, styles[`tint-${windowTone(value)}`])}>{formatAvailability(value)}</dd>
              </div>
            );
          })}
        </dl>
      </div>
      <ol className={styles.bars} aria-label={t("reports.uptime.bars", { name })}>
        {row.days.map((day) => {
          const label = barLabel(day);
          return <li key={day.date} className={cx(styles.bar, styles[`fill-${toneOf(day)}`])} title={label} aria-label={label} />;
        })}
      </ol>
      {period?.found && <PeriodLine name={period.name} found={period.found} />}
    </li>
  );
}

function Legend() {
  return (
    <ul className={styles.legend} aria-label={t("reports.uptime.legend")}>
      {(["up", "minor", "major", "none"] as const).map((tone) => (
        <li key={tone}>
          <span className={cx(styles.swatch, styles[`fill-${tone}`])} aria-hidden="true" />
          {t(`reports.uptime.legend.${tone}`)}
        </li>
      ))}
    </ul>
  );
}

/** The uptime of the reader's customer, or of everything, refreshed now and then. */
export function useUptime(customerId: string | null) {
  const zone = browserTimeZone();
  const fetcher = useCallback(
    (signal: AbortSignal) => api.uptime({ customer_id: customerId, time_zone: zone }, signal),
    [customerId, zone],
  );
  return usePolledData(fetcher, { intervalMs: REFRESH_MS });
}

/**
 * Uptime over 24 hours, 7, 30 and 90 days and by day (ADR-0172), each row
 * with the numbers of the chosen period's report under it when there is
 * one, so a server is listed once with all its numbers.
 */
export function UptimeSection({
  uptime,
  report,
  periodName,
}: {
  uptime: ReturnType<typeof useUptime>;
  report?: AvailabilityReport | null;
  periodName?: string;
}) {
  const { data, error, refresh } = uptime;
  const numbers = report ? periodNumbers(report) : null;
  const period = (row: UptimeRow) =>
    numbers && periodName ? { name: periodName, found: numbers.get(keyOf(row.kind, row.id, row.check)) } : null;
  return (
    <section aria-labelledby="uptime-title" className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 id="uptime-title" className={styles.title}>
          {t("reports.uptime.title")}
        </h2>
        <Legend />
      </div>
      <p className={admin.fieldNote}>{t(numbers ? "reports.uptime.notePeriod" : "reports.uptime.note")}</p>
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("reports.uptime.load")} error={describeError(error)} onRetry={refresh} />
      ) : data && data.rows.length === 0 ? (
        <p className={admin.fieldNote}>{t("reports.uptime.empty")}</p>
      ) : data ? (
        <ul className={styles.rows}>
          {data.rows.map((row) => (
            <Row key={`${row.kind}-${row.id}-${row.check ?? ""}`} row={row} period={period(row)} />
          ))}
        </ul>
      ) : null}
    </section>
  );
}
