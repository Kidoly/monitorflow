// Mirror display helpers without React (ADR-0030).

import type { Mirror, MirrorRunSummary } from "./api";

/** `https://deb.debian.org/debian/` reads `deb.debian.org/debian`. */
export function upstreamText(url: string): string {
  return url.replace(/^https?:\/\//, "").replace(/\/+$/, "");
}

/** "every 12 h", "every 15 min"; null is manual syncs only. */
export function intervalText(seconds: number | null): string {
  if (seconds === null) return "manual syncs only";
  for (const [size, unit] of [
    [86_400, "d"],
    [3_600, "h"],
    [60, "min"],
  ] as const) {
    if (seconds >= size && seconds % size === 0) return `every ${seconds / size} ${unit}`;
  }
  return `every ${Math.round(seconds / 60)} min`;
}

/**
 * "apt · deb.debian.org/debian · main contrib · amd64 arm64"; with
 * `distribution`, the suite follows the upstream as on the detail page.
 */
export function describeMirror(mirror: Mirror, { distribution = false } = {}): string {
  return [
    mirror.family,
    upstreamText(mirror.upstream_url),
    distribution && mirror.distribution,
    mirror.components.join(" "),
    mirror.architectures.length > 0 ? mirror.architectures.join(" ") : "all architectures",
  ]
    .filter(Boolean)
    .join(" · ");
}

/**
 * When the scheduler starts the next sync: the interval after the last
 * sync started (`store::due`). Null without an interval, while a run is
 * going, or when the last sync is unknown (`lastSync` undefined); `now` or
 * earlier means at the next poll.
 */
export function nextSync(
  mirror: Pick<Mirror, "sync_interval_s">,
  lastSync: Pick<MirrorRunSummary, "started_at"> | null | undefined,
  running: boolean,
): Date | null {
  if (mirror.sync_interval_s === null || running || lastSync === undefined) return null;
  if (lastSync === null) return new Date(0);
  return new Date(new Date(lastSync.started_at).getTime() + mirror.sync_interval_s * 1000);
}

const pad = (n: number) => String(n).padStart(2, "0");
const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "20:30" today, "23 Oct 02:00" another day, in local time. */
export function shortTime(date: Date, now: Date): string {
  const time = `${pad(date.getHours())}:${pad(date.getMinutes())}`;
  return date.toDateString() === now.toDateString() ? time : `${date.getDate()} ${MONTHS[date.getMonth()]} ${time}`;
}

/** "2026-10-22 08:00", local. */
export function minuteText(iso: string): string {
  const d = new Date(iso);
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** "42 s", "4 min 12 s", "9 min 02 s", "1 h 05 min". */
export function durationText(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min ${pad(s % 60)} s`;
  return `${Math.floor(s / 3600)} h ${pad(Math.floor((s % 3600) / 60))} min`;
}
