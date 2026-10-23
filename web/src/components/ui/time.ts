const pad = (n: number) => String(n).padStart(2, "0");

/** Local wall-clock time: "14:31:50". */
export function formatClock(date: Date): string {
  return `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** Local date and time, as in titles: "2026-10-22 14:31:53". */
export function formatExact(date: Date): string {
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${formatClock(date)}`;
}

const STEPS: [number, string][] = [
  [86_400, "d"],
  [3_600, "h"],
  [60, "min"],
];

/** `date` relative to `now` in the handoff's short form: "12 s ago", "41 min ago", "8 d ago". */
export function formatAgo(date: Date, now: Date): string {
  const seconds = Math.round((now.getTime() - date.getTime()) / 1000);
  const size = Math.abs(seconds);
  const [unit, name] = STEPS.find(([s]) => size >= s) ?? [1, "s"];
  const amount = `${Math.floor(size / unit)} ${name}`;
  return seconds >= 0 ? `${amount} ago` : `in ${amount}`;
}

/** A duration in the handoff's form: "45 s", "34 min", "2 h 52 min", "3 d 4 h". */
export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s} s`;
  const minutes = Math.floor(s / 60);
  if (minutes < 60) return `${minutes} min`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return minutes % 60 ? `${hours} h ${minutes % 60} min` : `${hours} h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days} d ${hours % 24} h` : `${days} d`;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "14 Oct": the day of `date`, without the year. */
export function formatShortDate(date: Date): string {
  return `${pad(date.getDate())} ${MONTHS[date.getMonth()]}`;
}

/**
 * A moment in a table relative to the day of `now`: "10:12 today",
 * "Yesterday 23:10", "20 Oct 08:00", or "2025-12-30 08:00" in another year.
 */
export function formatDay(date: Date, now: Date): string {
  const clock = formatClock(date).slice(0, 5);
  const day = (d: Date) => new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((day(now) - day(date)) / 86_400_000);
  if (days === 0) return `${clock} today`;
  if (days === 1) return `Yesterday ${clock}`;
  if (date.getFullYear() !== now.getFullYear()) return formatExact(date).slice(0, 16);
  return `${formatShortDate(date)} ${clock}`;
}

/** "11:40" on the day of `now`, else as `formatDay`: "Yesterday 23:10", "20 Oct 08:00". */
export function formatSince(date: Date, now: Date): string {
  const day = formatDay(date, now);
  return day.endsWith(" today") ? formatClock(date).slice(0, 5) : day;
}
