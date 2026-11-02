import type { FollowUp, FollowUpStep, FollowUpStepView, Incident } from "./api";

/** "bob", "bob and Slack #ops", "alice, bob and Slack #ops". */
export function listNames(names: string[]): string {
  if (names.length <= 1) return names[0] ?? "";
  return `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;
}

/** "1 minute", "15 minutes". */
export function minutesText(n: number): string {
  return n === 1 ? "1 minute" : `${n} minutes`;
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
  if (!followUp) return incident.severity === "warning" ? "Warnings are not followed up." : null;
  const who = listNames(followUp.next);
  switch (followUp.state) {
    case "waiting":
      return followUp.next.length > 0 && followUp.next_at
        ? `${who} will be told at ${clock(followUp.next_at)} if nobody acknowledges.`
        : "Nobody can be told next: the people of the next step have no way to be reached or cannot see this place.";
    case "silenced":
      return followUp.next.length > 0 && followUp.next_at
        ? `Silenced: after it ends, ${who} will be told at ${clock(followUp.next_at)} if nobody acknowledges.`
        : "Silenced: nobody is told until it ends.";
    case "stopped":
      switch (followUp.stop_reason) {
        case "finished":
          return "Everyone in the follow-up was told; nobody acknowledged.";
        case "not_critical":
          return "No longer critical: nobody else is told.";
        default:
          return null;
      }
  }
}

/** Who a step tells, in words: "the person on call of Ops, alice, Slack #ops and the administrators of the place". */
export function stepTargets(step: FollowUpStepView): string {
  const names = [
    ...step.on_call.map((s) => `the person on call of ${s.name ?? "a removed schedule"}`),
    ...step.users.map((u) => u.name ?? "a removed user"),
    ...step.channels.map((c) => c.name ?? "a removed channel"),
    ...(step.administrators ? ["the administrators of the place"] : []),
  ];
  return names.length ? listNames(names) : "nobody";
}

/** "After 15 minutes without acknowledgement, tell: …". */
export function stepSentence(step: FollowUpStepView): string {
  return `After ${minutesText(step.after_minutes)} without acknowledgement, tell ${stepTargets(step)}.`;
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
