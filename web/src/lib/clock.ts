// Short local dates and times of the administration pages' tables and facts.

import { t } from "@/i18n";

const pad = (n: number) => String(n).padStart(2, "0");

const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

/** "2026-10-27". */
export function formatDay(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

/** "07:40". */
export function formatHourMinute(date: Date): string {
  return `${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** "2026-10-15 07:40". */
export function formatMinute(date: Date): string {
  return `${formatDay(date)} ${formatHourMinute(date)}`;
}

/** A `datetime-local` value for `date`, in the browser's time zone: "2026-10-15T07:40". */
export function toLocalInput(date: Date): string {
  return `${formatDay(date)}T${formatHourMinute(date)}`;
}

/** The value of a `datetime-local` input as an ISO time, or null when empty or invalid. */
export function localToIso(value: string): string | null {
  if (!value) return null;
  const time = new Date(value);
  return Number.isNaN(time.getTime()) ? null : time.toISOString();
}

/** The browser's time zone, UTC when it names none. */
export function browserTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
}

/** Relative to `now`'s day: "14:32 today", "yesterday 22:05", else "2026-10-20 16:40". */
export function formatWhen(date: Date, now: Date): string {
  if (sameDay(date, now)) return t("ui.clock.today", { time: formatHourMinute(date) });
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(date, yesterday)) return t("ui.clock.yesterday", { time: formatHourMinute(date) });
  return formatMinute(date);
}

