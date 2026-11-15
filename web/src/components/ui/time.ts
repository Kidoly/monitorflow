import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

const pad = (n: number) => String(n).padStart(2, "0");

/** Local wall-clock time: "14:31:50". */
export function formatClock(date: Date): string {
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** Local date and time, as in titles: "2026-10-22 14:31:53". */
export function formatExact(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${formatClock(date)}`;
}

const STEPS = [
  [86_400, "day"],
  [3_600, "hour"],
  [60, "minute"],
] as const;

const unit = (name: "day" | "hour" | "minute" | "second") => t(`format.unit.${name}`);

/** `date` relative to `now` in the handoff's short form: "12 s ago", "41 min ago", "8 d ago". */
export function formatAgo(date: Date, now: Date): string {
  const seconds = Math.round((now.getTime() - date.getTime()) / 1000);
  const size = Math.abs(seconds);
  const [step, name] = STEPS.find(([s]) => size >= s) ?? [1, "second"];
  const amount = `${Math.floor(size / step)} ${unit(name)}`;
  return seconds >= 0 ? t("format.ago", { amount }) : t("format.in", { amount });
}

/** A duration in the handoff's form: "45 s", "34 min", "2 h 52 min", "3 d 4 h". */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s} ${unit("second")}`;
  const minutes = Math.floor(s / 60);
  if (minutes < 60) return `${minutes} ${unit("minute")}`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return minutes % 60 ? `${hours} ${unit("hour")} ${minutes % 60} ${unit("minute")}` : `${hours} ${unit("hour")}`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days} ${unit("day")} ${hours % 24} ${unit("hour")}` : `${days} ${unit("day")}`;
}

/** "14 Oct", "14 oct.": the day of `date`, without the year. */
export function formatShortDate(date: Date): string {
  // American English abbreviates every month to three letters; British English says "Sept".
  const locale = activeLocale() === "en" ? "en-US" : activeLocale();
  const month = date.toLocaleString(locale, { month: "short" });
  return t("format.shortDate", { day: pad(date.getDate()), month });
}

/**
 * A moment in a table relative to the day of `now`: "10:12 today",
 * "Yesterday 23:10", "20 Oct 08:00", or "2025-12-30 08:00" in another year.
 */
export function formatDay(date: Date, now: Date): string {
  const clock = formatClock(date).slice(0, 5);
  const days = daysBetween(date, now);
  if (days === 0) return t("format.today", { clock });
  if (days === 1) return t("format.yesterday", { clock });
  if (date.getFullYear() !== now.getFullYear()) return formatExact(date).slice(0, 16);
  return `${formatShortDate(date)} ${clock}`;
}

/** "11:40" on the day of `now`, else as `formatDay`: "Yesterday 23:10", "20 Oct 08:00". */
export function formatSince(date: Date, now: Date): string {
  return daysBetween(date, now) === 0 ? formatClock(date).slice(0, 5) : formatDay(date, now);
}

/** Calendar days from `date` to `now`, in local time. */
function daysBetween(date: Date, now: Date): number {
  const day = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  return Math.round((day(now) - day(date)) / 86_400_000);
}
