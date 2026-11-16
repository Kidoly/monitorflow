import type { Tone } from "@/components/ui/tone";
import { formatDay, formatExact } from "@/components/ui/time";
import { t } from "@/i18n";

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
  return role === "relay" ? t("authority.machine.relay") : t("authority.machine.server");
}

/** When the current step began: the switch, else the start. */
export function stepStart(rotation: CaRotation): string {
  return rotation.switched_at ?? rotation.started_at;
}

/** The headline of the page, and its tint. */
export function authorityStatus(view: CertificateAuthorityView): [string, Tone] {
  const rotation = view.rotation;
  if (rotation?.emergency) return [t("authority.status.emergency"), "bad"];
  if (rotation) return [t("authority.status.step", { step: rotation.stage === "introduced" ? "1" : "2" }), "info"];
  if (view.restored_retired_at) return [t("authority.status.restored"), "bad"];
  if (view.expires_soon) return [t("authority.status.expires", { date: formatDate(view.expires_at) }), "warn"];
  return [t("authority.status.valid", { date: formatDate(view.expires_at) }), "ok"];
}

/** The warning of an authority close to its end with no change started, else null. */
export function expiryWarning(view: CertificateAuthorityView, now: Date): string | null {
  if (!view.expires_soon || view.rotation) return null;
  const expires = formatDate(view.expires_at);
  return new Date(view.auto_change_at).getTime() > now.getTime()
    ? t("authority.expiry.auto", { expires, auto: formatDate(view.auto_change_at) })
    : t("authority.expiry.soon", { expires });
}

/** What the current step does, in one or two sentences. */
export function stepExplanation(rotation: CaRotation, now: Date): string {
  if (rotation.emergency) {
    return rotation.distrust_at
      ? t("authority.explain.emergencyOn", { day: formatDay(new Date(rotation.distrust_at), now) })
      : t("authority.explain.emergency");
  }
  return rotation.stage === "introduced" ? t("authority.explain.introduced") : t("authority.explain.switched");
}

/**
 * "312 of 315 servers and relays moved to the new certificate authority;
 * 3 have not connected since the change started."
 */
export function progressSentence(rotation: CaRotation): string {
  const { total, done } = rotation;
  const learned = rotation.stage === "introduced";
  if (total === 0) return t("authority.progress.none");
  if (done >= total) {
    return learned
      ? t("authority.progress.allLearned", { count: total })
      : t("authority.progress.allMoved", { count: total });
  }
  const left = total - done;
  const head = { done, total, machines: t("authority.machines", { count: total }) };
  const stillToGo = () =>
    learned
      ? t("authority.progress.learnedLeft", { ...head, left })
      : t("authority.progress.movedLeft", { ...head, left });
  // The list stops at 50: only a complete one tells how many are offline.
  if (left > rotation.waiting.length) return stillToGo();
  const since = new Date(stepStart(rotation)).getTime();
  const offline = rotation.waiting.filter((p) => !p.last_seen_at || new Date(p.last_seen_at).getTime() < since).length;
  if (offline === 0) return stillToGo();
  const when =
    rotation.stage === "introduced" || rotation.emergency
      ? t("authority.progress.sinceStart")
      : t("authority.progress.sinceSwitch");
  return learned
    ? t("authority.progress.learnedOffline", { ...head, count: offline, when })
    : t("authority.progress.movedOffline", { ...head, count: offline, when });
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
  const them = left === 0 ? t("authority.next.ready") : t("authority.next.cutOff", { count: left });
  if (rotation.stage === "introduced") {
    return {
      label: t("authority.next.switchLabel"),
      title: t("authority.next.switchTitle"),
      consequence: t("authority.next.switchConsequence", { them }),
      confirm: t("authority.next.switchConfirm"),
    };
  }
  return {
    label: t("authority.next.distrustLabel"),
    title: t("authority.next.distrustTitle"),
    consequence: t("authority.next.distrustConsequence", { them }),
    confirm: t("authority.next.distrustConfirm"),
  };
}

/** How to bring back a machine cut off by a change, keeping its name and history. */
export const reinstallHint = (): string => t("authority.reinstall");

/** "Stops being waited for on 2026-11-10", or nothing for a machine no longer waited for. */
export function waitedUntil(peer: CaPeerState): string {
  return t("authority.waitedUntil", { date: formatDate(peer.not_after) });
}

/**
 * The one-line notice every page shows global administrators, or null:
 * an emergency change, a restored retired authority, or one close to its end.
 */
export function authorityNotice(view: CertificateAuthorityView): string | null {
  if (view.rotation?.emergency) return t("authority.notice.emergency");
  if (view.restored_retired_at) return t("authority.notice.restored");
  if (view.cut_off.length > 0) return t("authority.notice.cutOff", { count: view.cut_off.length });
  if (view.expires_soon && !view.rotation) return t("authority.notice.expires", { date: formatDate(view.expires_at) });
  return null;
}
