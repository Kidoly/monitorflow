// Mirror display helpers without React (ADR-0030).

import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";

import type { Mirror, MirrorRunSummary } from "./api";
import { formatHourMinute, formatMinute } from "./clock";

/** `https://deb.debian.org/debian/` reads `deb.debian.org/debian`. */
export function upstreamText(url: string): string {
  return url.replace(/^https?:\/\//, "").replace(/\/+$/, "");
}

/** What retention keeps (ADR-0074), for the Mirrors pages. */
export function retentionText(keep: number): string {
  return t("mirrors.retention", { keep });
}

/** "every 12 h", "every 15 min"; null is manual syncs only. */
export function intervalText(seconds: number | null): string {
  if (seconds === null) return t("mirrors.manualOnly");
  for (const [size, unit] of [
    [86_400, "day"],
    [3_600, "hour"],
    [60, "minute"],
  ] as const) {
    if (seconds >= size && seconds % size === 0) {
      return t("mirrors.every", { amount: `${seconds / size} ${t(`format.unit.${unit}`)}` });
    }
  }
  return t("mirrors.every", { amount: `${Math.round(seconds / 60)} ${t("format.unit.minute")}` });
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
    mirror.architectures.length > 0 ? mirror.architectures.join(" ") : t("mirrors.allArchitectures"),
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

/** "20:30" today, "23 Oct 02:00" another day ("23 oct. 02:00" in French), in local time. */
export function shortTime(date: Date, now: Date): string {
  const time = formatHourMinute(date);
  if (date.toDateString() === now.toDateString()) return time;
  // American English abbreviates every month to three letters; British English says "Sept".
  const locale = activeLocale() === "en" ? "en-US" : activeLocale();
  const month = date.toLocaleString(locale, { month: "short" });
  return `${t("format.shortDate", { day: String(date.getDate()), month })} ${time}`;
}

/** "2026-10-22 08:00", local. */
export function minuteText(iso: string): string {
  return formatMinute(new Date(iso));
}

/** "42 s", "4 min 12 s", "9 min 02 s", "1 h 05 min". */
export function durationText(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  const [sec, min, hour] = [t("format.unit.second"), t("format.unit.minute"), t("format.unit.hour")];
  if (s < 60) return `${s} ${sec}`;
  if (s < 3600) return `${Math.floor(s / 60)} ${min} ${pad(s % 60)} ${sec}`;
  return `${Math.floor(s / 3600)} ${hour} ${pad(Math.floor((s % 3600) / 60))} ${min}`;
}
