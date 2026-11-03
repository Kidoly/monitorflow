import type { Tone } from "@/components/ui/tone";
import { formatDay, formatExact } from "@/components/ui/time";

import type { CaPeerState, CaRotation, CertificateAuthorityView } from "./api";

/**
 * Words of the certificate authority page (ADR-0088). Screens say
 * "certificate authority", "server" for an agent machine and "change", never
 * the internal "CA", "peer" or "rotation".
 */

/** Local date of `iso`: "2027-10-22". */
export function formatDate(iso: string): string {
  return formatExact(new Date(iso)).slice(0, 10);
}

/** "Server" or "Relay". */
export function machineWord(role: CaPeerState["role"]): string {
  return role === "relay" ? "Relay" : "Server";
}

/** When the current step began: the switch, else the start. */
export function stepStart(rotation: CaRotation): string {
  return rotation.switched_at ?? rotation.started_at;
}

/** The headline of the page, and its tint. */
export function authorityStatus(view: CertificateAuthorityView): [string, Tone] {
  const rotation = view.rotation;
  if (rotation?.emergency) return ["Emergency change under way", "bad"];
  if (rotation) return [`Change under way: step ${rotation.stage === "introduced" ? 1 : 2} of 2`, "info"];
  if (view.restored_retired_at) return ["A retired certificate authority is back", "bad"];
  if (view.expires_soon) return [`Expires on ${formatDate(view.expires_at)}`, "warn"];
  return [`Valid until ${formatDate(view.expires_at)}`, "ok"];
}

/** The warning of an authority close to its end with no change started, else null. */
export function expiryWarning(view: CertificateAuthorityView, now: Date): string | null {
  if (!view.expires_soon || view.rotation) return null;
  const auto =
    new Date(view.auto_change_at).getTime() > now.getTime()
      ? ` If nobody does, the change starts by itself on ${formatDate(view.auto_change_at)}.`
      : " The change starts by itself within the next minutes.";
  return (
    `The certificate authority expires on ${formatDate(view.expires_at)}. ` +
    `Change it now: one click, nothing to do on each server.${auto}`
  );
}

/** What the current step does, in one or two sentences. */
export function stepExplanation(rotation: CaRotation, now: Date): string {
  if (rotation.emergency) {
    const end = rotation.distrust_at
      ? `on ${formatDay(new Date(rotation.distrust_at), now)}`
      : "24 hours after the change started";
    return (
      "Every server and relay gets a certificate from the new certificate authority within the hour. " +
      `The old one stops being trusted ${end}, or as soon as every machine moved. ` +
      "Machines that miss it must be installed again."
    );
  }
  if (rotation.stage === "introduced") {
    return (
      "Step 1 of 2: every server and relay learns the new certificate authority over its secure connection, " +
      "within about an hour. The next step starts by itself once every machine has."
    );
  }
  return (
    "Step 2 of 2: the new certificate authority gives every server and relay a new certificate, " +
    "within about an hour. The old one is retired by itself once every machine moved. " +
    "Install commands made before the switch no longer work: copy a new one from the Add servers page."
  );
}

const plural = (n: number, one: string, many: string) => (n === 1 ? one : many);

/**
 * "312 of 315 servers and relays moved to the new certificate authority;
 * 3 have not connected since the change started."
 */
export function progressSentence(rotation: CaRotation): string {
  const { total, done } = rotation;
  const verb =
    rotation.stage === "introduced" ? "learned the new certificate authority" : "moved to the new certificate authority";
  if (total === 0) return "No server or relay to wait for.";
  const machines = plural(total, "server or relay", "servers and relays");
  if (done >= total) return `All ${total} ${machines} ${verb}.`;
  const left = total - done;
  const head = `${done} of ${total} ${machines} ${verb}`;
  // The list stops at 50: only a complete one tells how many are offline.
  if (left > rotation.waiting.length) return `${head}; ${left} still to go.`;
  const since = new Date(stepStart(rotation)).getTime();
  const offline = rotation.waiting.filter((p) => !p.last_seen_at || new Date(p.last_seen_at).getTime() < since).length;
  if (offline === 0) return `${head}; ${left} still to go.`;
  const when = rotation.stage === "introduced" || rotation.emergency ? "the change started" : "the switch";
  return `${head}; ${offline} ${plural(offline, "has", "have")} not connected since ${when}.`;
}

export interface NextStep {
  /** The trigger button. */
  label: string;
  title: string;
  consequence: string;
  confirm: string;
}

/** The button that takes the next step without waiting for the last machines. */
export function nextStep(rotation: CaRotation): NextStep {
  const left = Math.max(0, rotation.total - rotation.done);
  const them =
    left === 0
      ? "Every server and relay is ready."
      : `The ${left} ${plural(left, "server or relay", "servers and relays")} not done yet will not be able to connect: ` +
        "each must be installed again.";
  if (rotation.stage === "introduced") {
    return {
      label: "Switch now…",
      title: "Switch to the new certificate authority now?",
      consequence: `It gives every machine a new certificate without waiting. ${them}`,
      confirm: "Switch now",
    };
  }
  return {
    label: "Stop trusting the old authority now…",
    title: "Stop trusting the old certificate authority now?",
    consequence: `It retires the old certificate authority without waiting. ${them}`,
    confirm: "Stop trusting it now",
  };
}

/** How to bring back a machine cut off by a change, keeping its name and history. */
export const REINSTALL_HINT =
  "Install it again with a single-use token for its name, under Show single-use tokens on the Add servers page, to keep its history. " +
  "An install command would add it as a new machine.";

/** "Stops being waited for on 2026-11-10", or nothing for a machine no longer waited for. */
export function waitedUntil(peer: CaPeerState): string {
  return `Stops being waited for on ${formatDate(peer.not_after)}`;
}

/**
 * The one-line notice every page shows global administrators, or null:
 * an emergency change, a restored retired authority, or one close to its end.
 */
export function authorityNotice(view: CertificateAuthorityView): string | null {
  if (view.rotation?.emergency) return "An emergency change of the certificate authority is under way.";
  if (view.restored_retired_at) {
    return "A restore brought back a certificate authority this server had retired: servers and relays that moved cannot connect.";
  }
  if (view.cut_off.length > 0) {
    return `${view.cut_off.length} ${plural(view.cut_off.length, "server or relay needs", "servers and relays need")} a new install after a change of certificate authority.`;
  }
  if (view.expires_soon && !view.rotation) return `The certificate authority expires on ${formatDate(view.expires_at)}.`;
  return null;
}
