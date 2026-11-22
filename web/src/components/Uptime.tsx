"use client";

import { useCallback } from "react";

import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";
import { type UptimeDay, type UptimeRow, api, describeError, isUnauthorized } from "@/lib/api";
import { browserTimeZone } from "@/lib/onCall";
import { formatAvailability } from "@/lib/reports";
import { usePolledData } from "@/lib/usePolledData";
import { cx } from "@/components/ui/cx";
import { ErrorState } from "@/components/ui/PageState";
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

/** One row: what it is, its four windows, and its 90 days as bars. */
function Row({ row }: { row: UptimeRow }) {
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
                <dd className={cx(styles.percent, styles[windowTone(value)])}>
                  {formatAvailability(value)}
                </dd>
              </div>
            );
          })}
        </dl>
      </div>
      <ol className={styles.bars} aria-label={t("reports.uptime.bars", { name })}>
        {row.days.map((day) => {
          const label = barLabel(day);
          return <li key={day.date} className={cx(styles.bar, styles[toneOf(day)])} title={label} aria-label={label} />;
        })}
      </ol>
    </li>
  );
}

function Legend() {
  return (
    <ul className={styles.legend} aria-label={t("reports.uptime.legend")}>
      {(["up", "minor", "major", "none"] as const).map((tone) => (
        <li key={tone}>
          <span className={cx(styles.swatch, styles[tone])} aria-hidden="true" />
          {t(`reports.uptime.legend.${tone}`)}
        </li>
      ))}
    </ul>
  );
}

/** Reports' first section: uptime over 24 hours, 7, 30 and 90 days, and by day. */
export function UptimeSection({ customerId }: { customerId: string | null }) {
  const zone = browserTimeZone();
  const fetcher = useCallback(
    (signal: AbortSignal) => api.uptime({ customer_id: customerId, time_zone: zone }, signal),
    [customerId, zone],
  );
  const { data, error, refresh } = usePolledData(fetcher, { intervalMs: REFRESH_MS });
  return (
    <section aria-labelledby="uptime-title" className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 id="uptime-title" className={styles.title}>
          {t("reports.uptime.title")}
        </h2>
        <Legend />
      </div>
      <p className={admin.fieldNote}>{t("reports.uptime.note")}</p>
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("reports.uptime.load")} error={describeError(error)} onRetry={refresh} />
      ) : data && data.rows.length === 0 ? (
        <p className={admin.fieldNote}>{t("reports.uptime.empty")}</p>
      ) : data ? (
        <ul className={styles.rows}>
          {data.rows.map((row) => (
            <Row key={`${row.kind}-${row.id}-${row.check ?? ""}`} row={row} />
          ))}
        </ul>
      ) : null}
    </section>
  );
}
