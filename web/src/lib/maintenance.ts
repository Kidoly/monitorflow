import { t } from "@/i18n";
import { activeLocale, dateLocale } from "@/i18n/locale";

import type { MaintenanceOccurrence, MaintenanceWindow, MaintenanceWindowRequest, SilencedCheck, WeekdayName } from "./api";

/** Monday first, as the API orders them. */
export const WEEKDAY_NAMES: WeekdayName[] = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];

/** The longest window, in minutes: a week. */
export const MAX_MINUTES = 7 * 24 * 60;

/** "Sunday", "dimanche": the name of a weekday in the active language. */
export function dayName(day: WeekdayName): string {
  // 2024-01-01 was a Monday.
  const index = WEEKDAY_NAMES.indexOf(day) + 1;
  return new Date(Date.UTC(2024, 0, index)).toLocaleString(activeLocale(), { weekday: "long", timeZone: "UTC" });
}

/** "January", "janvier". */
export function monthName(month: number): string {
  return new Date(Date.UTC(2024, month - 1, 1)).toLocaleString(activeLocale(), { month: "long", timeZone: "UTC" });
}

/** "Sunday and Monday", "dimanche et lundi". */
function list(items: string[]): string {
  return new Intl.ListFormat(activeLocale(), { type: "conjunction" }).format(items);
}

/** Minutes since midnight of "02:30". */
export function minutesOf(clock: string): number {
  const [h = 0, m = 0] = clock.split(":").map(Number);
  return h * 60 + m;
}

/** "02:00" plus `minutes`, on a 24-hour clock. */
function clockPlus(start: string, minutes: number): string {
  const total = (minutesOf(start) + minutes) % (24 * 60);
  return `${String(Math.floor(total / 60)).padStart(2, "0")}:${String(total % 60).padStart(2, "0")}`;
}

/** "02:00 to 04:00 (Europe/Paris)", "22:00 to 02:00 the next day (Europe/Paris)". */
export function describeTimes(rule: Pick<MaintenanceWindowRequest, "start_time" | "duration_minutes" | "time_zone">): string {
  const zone = rule.time_zone.replaceAll("_", " ");
  const startMinutes = minutesOf(rule.start_time);
  if (startMinutes === 0 && rule.duration_minutes === 24 * 60) return t("maintenance.times.allDay", { zone });
  const from = rule.start_time;
  const to = clockPlus(from, rule.duration_minutes);
  const days = Math.floor((startMinutes + rule.duration_minutes - 1) / (24 * 60));
  if (days === 0) return t("maintenance.times.sameDay", { from, to, zone });
  if (days === 1) return t("maintenance.times.nextDay", { from, to, zone });
  return t("maintenance.times.later", { from, to, zone, count: days });
}

/** "1", or in French "1er" for the first day of a month. */
function dayOfMonth(day: number): string {
  return activeLocale() === "fr" && day === 1 ? "1er" : String(day);
}

/**
 * A rule in plain words: "Every Sunday, 02:00 to 04:00 (Europe/Paris)",
 * "On the second Tuesday of every month, 22:00 to 02:00 the next day (Europe/Paris)".
 */
export function describeRule(
  rule: Pick<
    MaintenanceWindowRequest,
    "repeat" | "every" | "weekdays" | "month_day" | "month_week" | "months" | "start_time" | "duration_minutes" | "time_zone"
  >,
): string {
  const times = describeTimes(rule);
  const days = list(WEEKDAY_NAMES.filter((d) => rule.weekdays.includes(d)).map(dayName));
  switch (rule.repeat) {
    case "daily":
      return rule.every > 1 ? t("maintenance.rule.everyDays", { count: rule.every, times }) : t("maintenance.rule.daily", { times });
    case "weekly":
      if (rule.weekdays.length === 7 && rule.every === 1) return t("maintenance.rule.daily", { times });
      return rule.every > 1
        ? t("maintenance.rule.everyWeeks", { count: rule.every, days, times })
        : t("maintenance.rule.weekly", { days, times });
    case "monthly": {
      const some = rule.months.length > 0 && rule.months.length < 12;
      const months = list([...rule.months].sort((a, b) => a - b).map(monthName));
      if (rule.month_day) {
        const day = dayOfMonth(rule.month_day);
        return some ? t("maintenance.rule.monthDayIn", { day, months, times }) : t("maintenance.rule.monthDay", { day, times });
      }
      const week = t(`maintenance.week.${Math.min(5, Math.max(1, rule.month_week ?? 1)) as 1 | 2 | 3 | 4 | 5}`);
      const weekday = rule.weekdays[0] ? dayName(rule.weekdays[0]) : "";
      return some
        ? t("maintenance.rule.monthWeekIn", { week, weekday, months, times })
        : t("maintenance.rule.monthWeek", { week, weekday, times });
    }
  }
}

/** "Sunday 22 November", in the window's time zone. */
function dayIn(date: Date, zone: string): string {
  return date.toLocaleString(dateLocale(), { weekday: "long", day: "numeric", month: "long", timeZone: zone });
}

/** "02:00", in the window's time zone. */
function clockIn(date: Date, zone: string): string {
  return date.toLocaleString("en-GB", { hour: "2-digit", minute: "2-digit", hourCycle: "h23", timeZone: zone });
}

/**
 * One window in the window's own time zone: "Sunday 22 November, 02:00 to 04:00",
 * or "Saturday 28 March, 22:00 to Sunday 29 March, 06:00" when it ends on another day.
 */
export function describeOccurrence(occurrence: MaintenanceOccurrence, zone: string): string {
  const start = new Date(occurrence.starts_at);
  const end = new Date(occurrence.ends_at);
  const day = dayIn(start, zone);
  const endDay = dayIn(end, zone);
  const from = clockIn(start, zone);
  const to = clockIn(end, zone);
  return day === endDay
    ? t("maintenance.occurrence.sameDay", { day, from, to })
    : t("maintenance.occurrence.otherDay", { day, from, endDay, to });
}

/** "Next: Sunday 22 November, 02:00 to 04:00", the one to come after any running now. */
export function describeNext(window: Pick<MaintenanceWindow, "next" | "time_zone">, now: Date): string {
  const next = window.next.find((o) => new Date(o.starts_at) > now);
  return next ? t("maintenance.next", { when: describeOccurrence(next, window.time_zone) }) : t("maintenance.nextNone");
}

/** "service_nginx", "disk /var": a check as people pick it. */
export function checkName(check: SilencedCheck): string {
  return check.instance ? `${check.name} ${check.instance}` : check.name;
}

/** "web-1, db-1 and 3 more", up to `shown` names. */
function names(items: string[], shown = 3): string {
  if (items.length <= shown) return list(items);
  return t("maintenance.what.more", { names: items.slice(0, shown).join(", "), count: items.length - shown });
}

/** What a window covers: "All of site paris", "web-1 and db-1", "web-1: service_nginx". */
export function describeWhat(window: Pick<MaintenanceWindow, "site_name" | "servers" | "checks" | "whole_site">): string {
  const what =
    window.whole_site ?? window.servers.length === 0
      ? t("maintenance.what.site", { site: window.site_name })
      : window.servers.length === 0
        ? t("maintenance.what.none")
        : t("maintenance.what.servers", { servers: names(window.servers.map((s) => s.hostname)) });
  return window.checks?.length ? t("maintenance.what.checks", { what, checks: names(window.checks.map(checkName)) }) : what;
}

/** Whether a silence of `checks` covers the check `name` of `instance`, as the server decides (ADR-0151). */
export function coversCheck(checks: SilencedCheck[], name: string, instance: string): boolean {
  return checks.some((c) => c.name === name && (c.instance === "" || c.instance === instance));
}
