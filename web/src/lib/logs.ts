import type { LogLine } from "./api";
import type { Tone } from "@/components/ui/tone";

/** The window the Logs tab shows when no time is chosen: the last hour. */
export const LIVE_WINDOW_MS = 60 * 60 * 1000;
/** The API's longest range, less a day so "to now" stays under it. */
export const MAX_WINDOW_MS = 30 * 24 * 60 * 60 * 1000;
/** Lines loaded at first; "Show older" adds as many. */
export const LOG_PAGE = 200;
/** The API's largest limit. */
export const MAX_LOG_LINES = 1000;

const PRIORITIES = ["Emergency", "Alert", "Critical", "Error", "Warning", "Notice", "Info", "Debug"];

/** The syslog priority in a word: 3 is "Error". */
export function priorityWord(priority: number | null): string {
  return priority === null ? "Unknown" : (PRIORITIES[priority] ?? "Unknown");
}

/** Errors and worse are bad, warnings warn, the rest is plain. */
export function priorityTone(priority: number | null): Tone {
  if (priority === null) return "neutral";
  if (priority <= 3) return "bad";
  return priority === 4 ? "warn" : "neutral";
}

/** What a line came from: its unit, else its identifier. */
export function lineSource(line: LogLine): string {
  return line.unit ?? line.identifier ?? "";
}

/**
 * The window to load from the `from` and `to` query parameters: the last
 * hour without them, to now with `from` alone. Invalid dates count as
 * absent, and the window is cut to the API's longest range. `live` says
 * whether it ends now, so it is worth refreshing.
 */
export function logWindow(from: string | null, to: string | null, now: Date): { start: Date; end: Date; live: boolean } {
  const parse = (value: string | null) => {
    const ms = value ? Date.parse(value) : NaN;
    return Number.isNaN(ms) ? null : new Date(ms);
  };
  const end = parse(to) ?? now;
  const live = parse(to) === null;
  let start = parse(from) ?? new Date(end.getTime() - LIVE_WINDOW_MS);
  if (start >= end) start = new Date(end.getTime() - LIVE_WINDOW_MS);
  if (end.getTime() - start.getTime() > MAX_WINDOW_MS) start = new Date(end.getTime() - MAX_WINDOW_MS);
  return { start, end, live };
}
