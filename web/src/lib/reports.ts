import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

import type { AvailabilityCause } from "./api";
import { formatDay } from "./clock";

/** The periods the Reports page offers (ADR-0155). */
export type PeriodChoice = "thisMonth" | "lastMonth" | "last7" | "last30" | "custom";

export const PERIOD_CHOICES: PeriodChoice[] = ["thisMonth", "lastMonth", "last7", "last30", "custom"];

const DAY_MS = 86_400_000;

/** `date` as the value of a date input: "2026-10-04", local. */
export function dateInput(date: Date): string {
  return formatDay(date);
}

/** Local midnight of a date input's value, or null. */
function midnight(value: string): Date | null {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return null;
  return new Date(Number(match[1]), Number(match[2]) - 1, Number(match[3]));
}

/**
 * The period of `choice` at `now`, in local time: this month so far, last
 * month, the last 7 or 30 days, or `custom` from the first day to the end
 * of the last one. Null when the custom days say nothing sensible.
 */
export function periodOf(
  choice: PeriodChoice,
  now: Date,
  custom: { from: string; to: string } = { from: "", to: "" },
): { from: Date; to: Date } | null {
  const monthStart = new Date(now.getFullYear(), now.getMonth(), 1);
  switch (choice) {
    case "thisMonth":
      return { from: monthStart, to: now };
    case "lastMonth":
      return { from: new Date(now.getFullYear(), now.getMonth() - 1, 1), to: monthStart };
    case "last7":
      return { from: new Date(now.getTime() - 7 * DAY_MS), to: now };
    case "last30":
      return { from: new Date(now.getTime() - 30 * DAY_MS), to: now };
    case "custom": {
      const from = midnight(custom.from);
      const last = midnight(custom.to);
      if (!from || !last) return null;
      const to = new Date(last.getFullYear(), last.getMonth(), last.getDate() + 1);
      if (to <= from || from >= now) return null;
      return { from, to: to > now ? now : to };
    }
  }
}

/**
 * A percentage with three decimals, never rounded up to 100: 99.9996 is
 * "99.999 %". Null is "Not measured", when all the time was planned
 * maintenance.
 */
export function formatAvailability(value: number | null): string {
  if (value === null) return t("reports.notMeasured");
  return formatPercent(Math.floor(value * 1000) / 1000, 3);
}

/** A target as set: "99.9 %". */
export function formatTarget(value: number): string {
  return formatPercent(value, 0, 3);
}

function formatPercent(value: number, min: number, max = min): string {
  const text = value.toLocaleString(activeLocale(), { minimumFractionDigits: min, maximumFractionDigits: max, useGrouping: false });
  return t("reports.percent", { value: text });
}

/** What a cause was, in a few words: "web-1: check disk (/)". */
export function describeCause(cause: AvailabilityCause): string {
  const check = cause.check ? (cause.instance ? `${cause.check} (${cause.instance})` : cause.check) : "";
  switch (cause.kind) {
    case "offline":
      return t("reports.cause.offline", { server: cause.server_name });
    case "unreachable":
      return cause.via
        ? t("reports.cause.unreachableVia", { server: cause.server_name, via: cause.via })
        : t("reports.cause.unreachable", { server: cause.server_name });
    case "check":
      return t("reports.cause.check", { server: cause.server_name, check });
    case "member":
      return check ? t("reports.cause.check", { server: cause.server_name, check }) : cause.server_name;
  }
}
