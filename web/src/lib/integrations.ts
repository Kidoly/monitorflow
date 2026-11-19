import { type PlainKey, t } from "@/i18n";

import type { IntegrationDelivery, IntegrationEndpoint } from "./api";

export { localToIso } from "./clock";

/** The receiver guide: payload, headers and how to check the signature (ADR-0102). */
export const INTEGRATIONS_GUIDE =
  "https://github.com/Kidoly/monitorflow/blob/main/docs/integrations.md";

type EventGroup = "incidents" | "servers" | "updates" | "campaigns" | "approvals";

/** Events by what they are about, in the API's order; their words are `integrations.event.<event>`. */
export const EVENT_GROUPS: { group: EventGroup; events: { event: string }[] }[] = [
  {
    group: "incidents",
    events: [
      { event: "incident.opened" },
      { event: "incident.escalated" },
      { event: "incident.acknowledged" },
      { event: "incident.resolved" },
    ],
  },
  {
    group: "servers",
    events: [{ event: "server.added" }, { event: "server.removed" }, { event: "server.offline" }, { event: "server.online" }],
  },
  {
    group: "updates",
    events: [{ event: "update.started" }, { event: "update.succeeded" }, { event: "update.failed" }],
  },
  {
    group: "campaigns",
    events: [{ event: "campaign.started" }, { event: "campaign.succeeded" }, { event: "campaign.failed" }],
  },
  {
    group: "approvals",
    events: [{ event: "approval.waiting" }, { event: "approval.decided" }],
  },
];

/** The title of a group of events, in the active language. */
export const groupTitle = (group: EventGroup) => t(`integrations.group.${group}`);

/** The words of a known event within its group: "Opened". */
export const eventLabel = (event: string) => t(`integrations.event.${event}` as PlainKey);

/** "Incidents: Opened" for an event, the raw name for one this page does not know. */
export function eventName(event: string): string {
  if (event === "test") return t("integrations.event.test");
  for (const group of EVENT_GROUPS) {
    if (group.events.some((e) => e.event === event)) {
      return t("integrations.eventOfGroup", { group: groupTitle(group.group), events: eventLabel(event) });
    }
  }
  return event;
}

/** `events` by group: a whole group by its title, part of one as "Servers: Added, Removed". */
function byGroup(events: string[]): string[] {
  return EVENT_GROUPS.flatMap((group) => {
    const chosen = group.events.filter((e) => events.includes(e.event));
    if (chosen.length === 0) return [];
    if (chosen.length === group.events.length) return [groupTitle(group.group)];
    return [
      t("integrations.eventOfGroup", {
        group: groupTitle(group.group),
        events: chosen.map((e) => eventLabel(e.event)).join(", "),
      }),
    ];
  });
}

/** What an integration asks for, in a few words: all, all but a few, or by group. */
export function eventsSummary(events: string[], all: string[]): string {
  const missing = all.filter((e) => !events.includes(e));
  if (missing.length === 0) return t("integrations.allEvents");
  if (missing.length <= 3) return t("integrations.allEventsBut", { events: byGroup(missing).join("; ") });
  return byGroup(events).join("; ");
}

/** The state of an integration in one word and its tone. */
export function stateLabel(
  endpoint: Pick<IntegrationEndpoint, "state" | "failing_since">,
): {
  text: string;
  tone: "ok" | "warn" | "bad" | "neutral";
} {
  if (endpoint.state === "paused") return { text: t("integrations.state.paused"), tone: "neutral" };
  if (endpoint.state === "disabled") return { text: t("integrations.state.disabled"), tone: "bad" };
  return endpoint.failing_since
    ? { text: t("integrations.state.failing"), tone: "warn" }
    : { text: t("integrations.state.on"), tone: "ok" };
}

/** How a delivery went, in a few words. */
export function deliveryResult(delivery: IntegrationDelivery): string {
  const code = delivery.last_status ? ` (${delivery.last_status})` : "";
  const count = delivery.attempts;
  if (delivery.state === "delivered") return t("integrations.delivered", { code });
  if (delivery.state === "failed") return t("integrations.gaveUp", { count, code });
  if (count === 0) return t("integrations.waitingToSend");
  return t("integrations.tried", { count, code });
}

