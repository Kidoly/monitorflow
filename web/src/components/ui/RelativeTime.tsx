import { formatAgo, formatClock, formatExact } from "./time";

/**
 * `<time datetime title>` reading "12 s ago", with the exact time in the
 * title and in the accessible name. `now` is the time of the data (the
 * last successful refresh), so the text does not count on by itself; with
 * `stale`, it says so: "8 s ago (at 14:31:50)".
 */
export function RelativeTime({ value, now, stale }: { value: string | Date; now: Date; stale?: boolean }) {
  const date = typeof value === "string" ? new Date(value) : value;
  if (Number.isNaN(date.getTime())) return <>—</>;
  const exact = formatExact(date);
  return (
    <time dateTime={date.toISOString()} title={exact}>
      {formatAgo(date, now)}
      <span className="visually-hidden">, {exact}</span>
      {stale && <span style={{ fontSize: 12 }}> (at {formatClock(now)})</span>}
    </time>
  );
}
