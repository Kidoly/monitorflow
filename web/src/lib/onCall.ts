import { t } from "@/i18n";
import { activeLocale, dateLocale } from "@/i18n/locale";

import type { OnCallProblem, OnCallSchedule, OnCallTurn } from "./api";

export { browserTimeZone, toLocalInput } from "./clock";

/** ISO weekdays, 1 for Monday. */
export const WEEKDAYS = [1, 2, 3, 4, 5, 6, 7] as const;

/** The name of ISO weekday `day` in the active language: "Monday", "lundi". */
export function weekdayName(day: number): string {
  // 2024-01-01 was a Monday.
  return new Date(Date.UTC(2024, 0, day)).toLocaleString(activeLocale(), { weekday: "long", timeZone: "UTC" });
}

/** "Every Monday at 09:00, Europe/Paris time". */
export function handoffRule(schedule: Pick<OnCallSchedule, "rotation" | "handoff_day" | "handoff_time" | "time_zone">): string {
  const time = schedule.handoff_time;
  const zone = schedule.time_zone.replaceAll("_", " ");
  return schedule.rotation === "weekly" && schedule.handoff_day
    ? t("onCall.handoffWeekly", { day: weekdayName(schedule.handoff_day), time, zone })
    : t("onCall.handoffDaily", { time, zone });
}

/**
 * "acme / paris", "All of acme", "Everything": what a schedule covers. With
 * `multiCustomer` false (ADR-0104), "All sites" for a whole customer.
 */
export function placeName(
  schedule: Pick<OnCallSchedule, "customer_name" | "site_name">,
  severalCustomers: boolean,
  multiCustomer = true,
): string {
  if (schedule.site_name) return severalCustomers ? `${schedule.customer_name} / ${schedule.site_name}` : (schedule.site_name ?? "");
  if (!schedule.customer_name) return t("notifications.scope.everything");
  return multiCustomer ? t("notifications.scope.allOf", { name: schedule.customer_name }) : t("notifications.scope.allSites");
}

/** Why a turn is a gap, in plain words: "bob has not added a way to reach them". */
export function problemText(problem: OnCallProblem, username: string | null, place: string): string {
  const who = username ?? t("onCall.removedUser");
  const wide = place === t("notifications.scope.everything") || place === t("notifications.scope.allSites");
  switch (problem) {
    case "nobody":
      return t("onCall.problem.nobody");
    case "removed":
      return t("onCall.problem.removed");
    case "disabled":
      return t("onCall.problem.disabled", { who });
    case "no_access":
      return t("onCall.problem.noAccess", { who, place: wide ? place.toLowerCase() : place });
    case "no_contact":
      return t("onCall.problem.noContact", { who });
    case "no_mail_server":
      return t("onCall.problem.noMailServer", { who });
  }
}

/** The turns that are gaps, in order. */
export function gaps(schedule: Pick<OnCallSchedule, "turns">): OnCallTurn[] {
  return schedule.turns.filter((turn) => turn.problem !== null);
}

/** "Mon 9 Nov, 09:00", "lun. 9 nov., 09:00", in the browser's time zone. */
export function formatWhen(date: Date): string {
  return date.toLocaleString(dateLocale(), {
    weekday: "short",
    day: "numeric",
    month: "short",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });
}

/** Who is on call in a turn, for a sentence: "alice", or "nobody" in a gap without a person. */
export function who(turn: Pick<OnCallTurn, "username" | "user_id">): string {
  return turn.username ?? (turn.user_id ? t("onCall.removedUser") : t("onCall.nobody"));
}

/** Time zones to choose from, with `current` and UTC always among them. */
export function timeZones(current: string): string[] {
  const all: string[] =
    typeof Intl.supportedValuesOf === "function" ? Intl.supportedValuesOf("timeZone") : [];
  return [...new Set([current, "UTC", ...all])].sort((a, b) => a.localeCompare(b));
}

/** `items` with the one at `index` moved by `by` places, kept within bounds. */
export function move<T>(items: T[], index: number, by: number): T[] {
  const to = Math.max(0, Math.min(items.length - 1, index + by));
  const next = [...items];
  next.splice(to, 0, ...next.splice(index, 1));
  return next;
}
