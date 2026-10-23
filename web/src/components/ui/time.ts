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
