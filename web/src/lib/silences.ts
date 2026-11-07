import type { Silence } from "./api";

/** What a one-click silence says; the custom form lets people say more. */
export const DEFAULT_REASON = "Planned maintenance";

/** The longest silence the API accepts (ADR-0079). */
export const MAX_DAYS = 30;

const HOUR_MS = 3_600_000;

/** An hour after `now`. */
export function inOneHour(now: Date): Date {
  return new Date(now.getTime() + HOUR_MS);
}

/** 08:00 on the day after `now`, in the browser's time zone. */
export function tomorrowMorning(now: Date): Date {
  return new Date(now.getFullYear(), now.getMonth(), now.getDate() + 1, 8);
}

/** A `datetime-local` value as a date, or null when empty or malformed. */
export function fromLocalInput(value: string): Date | null {
  const date = new Date(value);
  return value && !Number.isNaN(date.getTime()) ? date : null;
}

/** "web-1", "site paris", "customer acme" ("all sites" for a single company, ADR-0104), "everything". */
export function silenceTitle(silence: Pick<Silence, "scope" | "target_name">, multiCustomer = true): string {
  const name = silence.target_name ?? "";
  switch (silence.scope) {
    case "server":
      return name;
    case "relay":
      return `relay ${name}`;
    case "site":
      return `site ${name}`;
    case "customer":
      return multiCustomer ? `customer ${name}` : "all sites";
    case "all":
      return "everything";
  }
}
