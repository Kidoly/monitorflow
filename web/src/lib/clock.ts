// Short local dates and times of the administration pages' tables and facts.

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

/** Relative to `now`'s day: "14:32 today", "yesterday 22:05", else "2026-10-20 16:40". */
export function formatWhen(date: Date, now: Date): string {
  if (sameDay(date, now)) return `${formatHourMinute(date)} today`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(date, yesterday)) return `yesterday ${formatHourMinute(date)}`;
  return formatMinute(date);
}

/** Whole days from `now` until `date`, rounded up: 4.2 days left gives 5. */
export function daysUntil(date: Date, now: Date): number {
  return Math.ceil((date.getTime() - now.getTime()) / 86_400_000);
}
