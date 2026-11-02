import type { OnCallProblem, OnCallSchedule, OnCallTurn } from "./api";

/** ISO weekdays, 1 for Monday. */
export const WEEKDAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/** "Every Monday at 09:00, Europe/Paris time". */
export function handoffRule(schedule: Pick<OnCallSchedule, "rotation" | "handoff_day" | "handoff_time" | "time_zone">): string {
  const day = schedule.rotation === "weekly" && schedule.handoff_day ? WEEKDAYS[schedule.handoff_day - 1] : "day";
  return `Hands off every ${day} at ${schedule.handoff_time}, ${schedule.time_zone.replaceAll("_", " ")} time`;
}

/** "acme / paris", "All of acme", "Everything": what a schedule covers. */
export function placeName(schedule: Pick<OnCallSchedule, "customer_name" | "site_name">, severalCustomers: boolean): string {
  if (schedule.site_name) return severalCustomers ? `${schedule.customer_name} / ${schedule.site_name}` : (schedule.site_name ?? "");
  return schedule.customer_name ? `All of ${schedule.customer_name}` : "Everything";
}

/** Why a turn is a gap, in plain words: "bob has not added a way to reach them". */
export function problemText(problem: OnCallProblem, username: string | null, place: string): string {
  const who = username ?? "a removed user";
  switch (problem) {
    case "nobody":
      return "Nobody is on this schedule";
    case "removed":
      return "This turn is a removed user's";
    case "disabled":
      return `${who}'s account is disabled`;
    case "no_access":
      return `${who} can no longer see ${place === "Everything" ? "everything" : place}`;
    case "no_contact":
      return `${who} has not added an email address or a chat`;
    case "no_mail_server":
      return `${who} has only an email address, and no email channel can send it`;
  }
}

/** The turns that are gaps, in order. */
export function gaps(schedule: Pick<OnCallSchedule, "turns">): OnCallTurn[] {
  return schedule.turns.filter((turn) => turn.problem !== null);
}

const WHEN = new Intl.DateTimeFormat("en-GB", {
  weekday: "short",
  day: "numeric",
  month: "short",
  hour: "2-digit",
  minute: "2-digit",
  hourCycle: "h23",
});

/** "Mon 9 Nov, 09:00", in the browser's time zone. */
export function formatWhen(date: Date): string {
  return WHEN.format(date);
}

/** Who is on call in a turn, for a sentence: "alice", or "nobody" in a gap without a person. */
export function who(turn: Pick<OnCallTurn, "username" | "user_id">): string {
  return turn.username ?? (turn.user_id ? "a removed user" : "nobody");
}

/** A `datetime-local` value for `date`, in the browser's time zone. */
export function toLocalInput(date: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** The browser's time zone, the default of a new schedule. */
export function browserTimeZone(): string {
  return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
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
