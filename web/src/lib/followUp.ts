import { t } from "@/i18n";

import type { FollowUp, FollowUpStep, FollowUpStepView, Incident } from "./api";

/** "bob", "bob and Slack #ops", "alice, bob and Slack #ops". */
export function listNames(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return t("followUp.and", { list: names.slice(0, -1).join(", "), last: names[names.length - 1]! });
}

/** "1 minute", "15 minutes". */
export function minutesText(n: number): string {
  return t("format.minutes", { count: n });
}

/**
 * Who is told next if nobody acknowledges, in one sentence (ADR-0082).
 * `clock` writes a time as the reader reads it, like "03:27". Null when
 * there is nothing to say: no follow-up and not an open warning.
 */
export function followUpLine(
  incident: Pick<Incident, "state" | "severity" | "acknowledged_at">,
  followUp: FollowUp | null | undefined,
  clock: (iso: string) => string,
): string | null {
  if (incident.state !== "open" || incident.acknowledged_at) return null;
  if (!followUp) return incident.severity === "warning" ? t("followUp.warnings") : null;
  const next = { count: followUp.next.length, who: listNames(followUp.next) };
  switch (followUp.state) {
    case "waiting":
      return followUp.next.length > 0 && followUp.next_at
        ? t("followUp.willBeTold", { ...next, time: clock(followUp.next_at) })
        : t("followUp.nobodyNext");
    case "silenced":
      return followUp.next.length > 0 && followUp.next_at
        ? t("followUp.silencedThen", { ...next, time: clock(followUp.next_at) })
        : t("followUp.silenced");
    case "stopped":
      switch (followUp.stop_reason) {
        case "finished":
          return t("followUp.finished");
        case "not_critical":
          return t("followUp.notCritical");
        default:
          return null;
      }
  }
}

/** Who a step tells, in words: "the person on call of Ops, alice, Slack #ops and the administrators of the place". */
export function stepTargets(step: FollowUpStepView): string {
  const names = [
    ...step.on_call.map((s) => t("followUp.personOnCall", { name: s.name ?? t("followUp.removedSchedule") })),
    ...step.users.map((u) => u.name ?? t("onCall.removedUser")),
    ...step.channels.map((c) => c.name ?? t("followUp.removedChannel")),
    ...(step.administrators ? [t("followUp.administrators")] : []),
  ];
  return names.length ? listNames(names) : t("onCall.nobody");
}

/** "After 15 minutes without acknowledgement, tell: …". */
export function stepSentence(step: FollowUpStepView): string {
  return t("followUp.step", { minutes: minutesText(step.after_minutes), targets: stepTargets(step) });
}

/** The step as saved from how it is shown. */
export function stepRequest(step: FollowUpStepView): FollowUpStep {
  return {
    after_minutes: step.after_minutes,
    on_call: step.on_call.map((s) => s.id),
    users: step.users.map((u) => u.id),
    channels: step.channels.map((c) => c.id),
    administrators: step.administrators,
  };
}

/** A place by its columns: a site, else a customer, else everything. */
export interface Place {
  customer_id: string | null;
  site_id: string | null;
}

/**
 * Whether a schedule or channel covering `choice` covers all of `place`,
 * so that a step of `place` may name it. `siteCustomer` maps each site to
 * its customer. UX only: the API decides.
 */
export function covers(choice: Place, place: Place, siteCustomer: Map<string, string>): boolean {
  if (choice.site_id) return choice.site_id === place.site_id;
  if (!choice.customer_id) return true;
  if (place.site_id) return siteCustomer.get(place.site_id) === choice.customer_id;
  return place.customer_id === choice.customer_id;
}
