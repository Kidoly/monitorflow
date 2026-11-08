import type { IntegrationDelivery, IntegrationEndpoint } from "./api";

/** The receiver guide: payload, headers and how to check the signature (ADR-0102). */
export const INTEGRATIONS_GUIDE =
  "https://github.com/Kidoly/monitorflow/blob/main/docs/integrations.md";

/** Events by what they are about, in plain words, in the API's order. */
export const EVENT_GROUPS: {
  title: string;
  events: { event: string; label: string }[];
}[] = [
  {
    title: "Incidents",
    events: [
      { event: "incident.opened", label: "Opened" },
      { event: "incident.escalated", label: "Became critical" },
      { event: "incident.acknowledged", label: "Acknowledged" },
      { event: "incident.resolved", label: "Resolved" },
    ],
  },
  {
    title: "Servers",
    events: [
      { event: "server.added", label: "Added" },
      { event: "server.removed", label: "Removed" },
      { event: "server.offline", label: "Stopped reporting" },
      { event: "server.online", label: "Reporting again" },
    ],
  },
  {
    title: "Updates of one server",
    events: [
      { event: "update.started", label: "Started" },
      { event: "update.succeeded", label: "Succeeded" },
      { event: "update.failed", label: "Did not succeed" },
    ],
  },
  {
    title: "Campaigns",
    events: [
      { event: "campaign.started", label: "Started" },
      { event: "campaign.succeeded", label: "Finished" },
      { event: "campaign.failed", label: "Did not finish" },
    ],
  },
  {
    title: "Approvals",
    events: [
      { event: "approval.waiting", label: "Waiting" },
      { event: "approval.decided", label: "Decided" },
    ],
  },
];

/** "Incidents: Opened" for an event, the raw name for one this page does not know. */
export function eventName(event: string): string {
  if (event === "test") return "Test";
  for (const group of EVENT_GROUPS) {
    const found = group.events.find((e) => e.event === event);
    if (found) return `${group.title}: ${found.label}`;
  }
  return event;
}

/** `events` by group: a whole group by its title, part of one as "Servers: Added, Removed". */
function byGroup(events: string[]): string[] {
  return EVENT_GROUPS.flatMap((group) => {
    const chosen = group.events.filter((e) => events.includes(e.event));
    if (chosen.length === 0) return [];
    if (chosen.length === group.events.length) return [group.title];
    return [`${group.title}: ${chosen.map((e) => e.label).join(", ")}`];
  });
}

/** What an integration asks for, in a few words: all, all but a few, or by group. */
export function eventsSummary(events: string[], all: string[]): string {
  const missing = all.filter((e) => !events.includes(e));
  if (missing.length === 0) return "All events";
  if (missing.length <= 3)
    return `All events but ${byGroup(missing).join("; ")}`;
  return byGroup(events).join("; ");
}

/** The state of an integration in one word and its tone. */
export function stateLabel(
  endpoint: Pick<IntegrationEndpoint, "state" | "failing_since">,
): {
  text: string;
  tone: "ok" | "warn" | "bad" | "neutral";
} {
  if (endpoint.state === "paused") return { text: "Paused", tone: "neutral" };
  if (endpoint.state === "disabled") return { text: "Turned off", tone: "bad" };
  return endpoint.failing_since
    ? { text: "Failing", tone: "warn" }
    : { text: "On", tone: "ok" };
}

/** How a delivery went, in a few words. */
export function deliveryResult(delivery: IntegrationDelivery): string {
  const code = delivery.last_status ? ` (${delivery.last_status})` : "";
  if (delivery.state === "delivered") return `Delivered${code}`;
  if (delivery.state === "failed")
    return `Gave up after ${delivery.attempts} tries${code}`;
  if (delivery.attempts === 0) return "Waiting to be sent";
  return `Tried ${delivery.attempts} ${delivery.attempts === 1 ? "time" : "times"}${code}, trying again`;
}

/** The value of a `datetime-local` input as an ISO time, or null when empty or invalid. */
export function localToIso(value: string): string | null {
  if (!value) return null;
  const time = new Date(value);
  return Number.isNaN(time.getTime()) ? null : time.toISOString();
}
