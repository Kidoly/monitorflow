// Times of the demo, relative to when the page loaded, so it never looks
// stale. Rounded to the minute so two answers agree.

/** When the demo started, in milliseconds, on a whole minute. */
export const START = Math.floor(Date.now() / 60_000) * 60_000;

const MINUTE = 60_000;

/** RFC 3339 for `minutes` before the demo started. */
export function ago(minutes: number): string {
  return new Date(START - minutes * MINUTE).toISOString();
}

/** RFC 3339 for `minutes` after the demo started. */
export function later(minutes: number): string {
  return new Date(START + minutes * MINUTE).toISOString();
}

export const HOUR = 60;
export const DAY = 24 * HOUR;
