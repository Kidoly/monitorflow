import { formatSeconds } from "./format";

/** Shortest and longest interval the API accepts (ADR-0097). */
export const MIN_INTERVAL_MS = 10_000;
export const MAX_INTERVAL_MS = 86_400_000;

/** The intervals offered in the list; "Other" takes the rest. */
export const INTERVAL_CHOICES = [10_000, 30_000, 60_000, 300_000, 900_000, 3_600_000];

/** Units of an interval typed by hand. */
export const INTERVAL_UNITS = [
  { label: "seconds", ms: 1_000 },
  { label: "minutes", ms: 60_000 },
  { label: "hours", ms: 3_600_000 },
] as const;

/** "every 30 s", "every 1 h". */
export function every(ms: number): string {
  return `every ${formatSeconds(ms / 1000)}`;
}

/** The interval typed as `amount` of `unitMs`, or why it is refused. */
export function customInterval(amount: string, unitMs: number): { ms: number } | { error: string } {
  const value = Number(amount.trim().replace(",", "."));
  const ms = Math.round(value * unitMs);
  if (amount.trim() === "" || !Number.isFinite(value) || ms < MIN_INTERVAL_MS || ms > MAX_INTERVAL_MS) {
    return { error: "Enter a value from 10 seconds to 24 hours." };
  }
  return { ms };
}

/** `ms` as an amount of the largest unit that divides it, to prefill "Other". */
export function splitInterval(ms: number): { amount: string; unitMs: number } {
  const unit = [...INTERVAL_UNITS].reverse().find((u) => ms % u.ms === 0) ?? INTERVAL_UNITS[0];
  return { amount: String(ms / unit.ms), unitMs: unit.ms };
}

/**
 * The timeout of a check without its own: a sixth of its interval, from
 * 10 s to 60 s, never longer than the interval. The server computes the
 * same (`monitorflow_sensors::default_timeout`) and sends it to the agent.
 */
export function defaultTimeoutMs(intervalMs: number): number {
  return Math.min(Math.min(Math.max(intervalMs / 6, 10_000), 60_000), intervalMs);
}

/**
 * Longest time, in seconds, from a problem starting to its alert opening
 * when the check runs every `intervalS`: the next check sees it, then the
 * alert's duration rounded up to whole checks (ADR-0097).
 */
export function alertDelayS(durationS: number, intervalS: number): number {
  return intervalS + Math.ceil(durationS / intervalS) * intervalS;
}

/**
 * What an alert lasting `durationS` does with checks every `intervalS`,
 * when that is notably later than it reads; null for checks every minute
 * or more often.
 */
export function slowAlertNote(durationS: number, intervalS: number | undefined): string | null {
  if (intervalS === undefined || intervalS <= 60) return null;
  return `Checked ${every(intervalS * 1000)}, so this alert opens up to ${formatSeconds(alertDelayS(durationS, intervalS))} after the problem starts.`;
}
