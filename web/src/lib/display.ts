import type { Panel } from "./api";
import { formatClock } from "@/components/ui/time";

/** The page that shows dashboards on a screen (ADR-0099). */
export const DISPLAY_PATH = "/display/";

/** How often the screen re-reads what it shows: edits and links turned off. */
export const VIEW_REFRESH_MS = 5 * 60_000;

/** How long the buttons stay after the mouse last moved. */
export const CONTROLS_MS = 3_000;

/** "Show on a screen" for the dashboard `id`, or a ready-made one. */
export function displayHref(dashboard: { id: string } | { ready: string }): string {
  return "id" in dashboard
    ? `${DISPLAY_PATH}?id=${encodeURIComponent(dashboard.id)}`
    : `${DISPLAY_PATH}?ready=${encodeURIComponent(dashboard.ready)}`;
}

/**
 * Rows of the three-column grid `panels` take, placed in order as CSS
 * grid places them: a panel that does not fit the rest of a row starts
 * the next one.
 */
export function gridRows(panels: Pick<Panel, "width">[]): number {
  let rows = 0;
  let used = 3;
  for (const { width } of panels) {
    if (used + width > 3) {
      rows += 1;
      used = 0;
    }
    used += width;
  }
  return Math.max(rows, 1);
}

/** Text size for `rows` rows: full size up to two, smaller beyond, never under half. */
export function textScale(rows: number): number {
  return Math.min(1, Math.max(0.5, 2.6 / rows));
}

/**
 * Whether what the screen shows is too old to pass for live: no answer
 * for more than twice the refresh period. Times are of a monotonic clock,
 * in milliseconds; `lastAnswer` null is before the first one.
 */
export function isStale(lastAnswer: number | null, now: number, refreshMs: number): boolean {
  return lastAnswer !== null && now - lastAnswer > 2 * refreshMs;
}

/** The banner of a stale screen, with the time of what it shows. */
export function staleText(dataTime: Date): string {
  return `Data from ${formatClock(dataTime).slice(0, 5)} — this screen has lost its connection to MonitorFlow. It keeps trying.`;
}

/** The page after `page` of `count`, back to the first after the last. */
export function nextPage(page: number, count: number): number {
  return count > 0 ? (page + 1) % count : 0;
}

/** Why a screen shows no dashboard, in plain words. */
export type ScreenProblem = "gone" | "nothing" | "missing";

export const PROBLEM_TEXT: Record<ScreenProblem, { title: string; text: string }> = {
  gone: {
    title: "This display link no longer works",
    text: "It was turned off or has expired. Ask an administrator for a new one.",
  },
  nothing: {
    title: "Nothing to show",
    text: "The person who made this display link can no longer show its dashboards. This screen tries again every few minutes.",
  },
  missing: {
    title: "Nothing to show",
    text: "This dashboard does not exist, or you cannot see it.",
  },
};
