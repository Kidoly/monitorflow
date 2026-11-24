import type { Incident, OnCallSchedule, SilenceRequest } from "./api";
import { isOffline } from "./incidents";

/** The web app's pages for phones (ADR-0186); the installed app opens here. */
export const MOBILE_PATH = "/mobile/";
export const MOBILE_ON_CALL_PATH = "/mobile/on-call/";
export const MOBILE_PHONE_PATH = "/mobile/phone/";

/** An incident's page on a phone; `silence` opens its silence choices. */
export function mobileIncidentHref(id: string, silence = false): string {
  return `/mobile/incident/?id=${encodeURIComponent(id)}${silence ? "&silence=1" : ""}`;
}

/** Whether the page runs as the installed app rather than in a browser tab. */
export function isStandalone(): boolean {
  if (typeof window === "undefined") return false;
  const nav = window.navigator as Navigator & { standalone?: boolean };
  return nav.standalone === true || window.matchMedia?.("(display-mode: standalone)").matches === true;
}

const FULL_VIEW = "mf-full-view";

/** Remembers, for this window, that the person chose the full interface over the phone pages. */
export function chooseFullView(): void {
  try {
    sessionStorage.setItem(FULL_VIEW, "1");
  } catch {
    // Storage off: the app goes back to its pages after the next sign-in.
  }
}

/** Whether "/" should go on to the phone pages: in the installed app, unless the person chose the full view. */
export function opensMobile(): boolean {
  if (!isStandalone()) return false;
  try {
    return sessionStorage.getItem(FULL_VIEW) !== "1";
  } catch {
    return true;
  }
}

/** Whether `userAgent` is an iPhone's or an iPad's, where push needs the app on the home screen. */
export function isApple(userAgent: string): boolean {
  return /iPhone|iPad|iPod/.test(userAgent) || (/Macintosh/.test(userAgent) && /Mobile/.test(userAgent));
}

/** A first name for this phone, which the person may change. */
export function phoneName(userAgent: string): string {
  if (/iPad/.test(userAgent)) return "iPad";
  if (isApple(userAgent)) return "iPhone";
  const model = /Android [\d.]+; ([^;)]+?)(?: Build\/[^;)]*)?\)/.exec(userAgent)?.[1]?.trim();
  if (model && model !== "K" && !/^wv$/i.test(model)) return model.slice(0, 80);
  if (/Android/.test(userAgent)) return "Android";
  return "Phone";
}

/** Where push stands in this browser. */
export type PushSupport = "ready" | "install" | "unsupported" | "denied";

export function pushSupport(env: {
  userAgent: string;
  standalone: boolean;
  hasPushManager: boolean;
  hasServiceWorker: boolean;
  permission: NotificationPermission | null;
}): PushSupport {
  // Safari offers push only to the app added to the home screen (iOS 16.4+).
  if (isApple(env.userAgent) && !env.standalone) return "install";
  if (!env.hasServiceWorker || !env.hasPushManager || env.permission === null) return "unsupported";
  if (env.permission === "denied") return "denied";
  return "ready";
}

/** `applicationServerKey` from its base64url text. */
export function base64UrlToBytes(text: string): Uint8Array<ArrayBuffer> {
  const base64 = text.replace(/-/g, "+").replace(/_/g, "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const out = new Uint8Array(new ArrayBuffer(binary.length));
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

/** How long a silence from a phone lasts, in minutes. */
export const SILENCE_MINUTES = [30, 60, 240, 1440] as const;

/**
 * The silence for `incident` lasting `minutes` from `now`: its check only
 * when `check` is known (ADR-0151), else its whole server or relay.
 */
export function silenceFor(
  incident: Pick<Incident, "server_id" | "relay_id" | "metric">,
  check: { name: string; instance: string } | null,
  minutes: number,
  now: Date,
  reason: string,
): SilenceRequest {
  const ends_at = new Date(now.getTime() + minutes * 60_000).toISOString();
  if (!incident.server_id) return { scope: "relay", target_id: incident.relay_id ?? "", ends_at, reason };
  const base = { scope: "server" as const, target_id: incident.server_id, ends_at, reason };
  return check && !isOffline(incident) ? { ...base, checks: [check] } : base;
}

/** Who is on call now in one schedule, for the phone's list. */
export interface OnCallNow {
  id: string;
  name: string;
  people: string[];
  /** Whether the signed-in person is one of them. */
  me: boolean;
  /** When this turn hands off; null when nobody is on call. */
  until: string | null;
  next: string[];
  gap: boolean;
}

export function onCallNow(schedules: OnCallSchedule[], meId: string): OnCallNow[] {
  const rows = schedules.map((s) => {
    const now = s.now_all.length > 0 ? s.now_all : s.now ? [s.now] : [];
    const next = s.next_all.length > 0 ? s.next_all : s.next ? [s.next] : [];
    return {
      id: s.id,
      name: s.name,
      people: now.map((turn) => turn.username).filter((name): name is string => name !== null),
      me: now.some((turn) => turn.user_id === meId),
      until: now[0]?.ends_at ?? null,
      next: next.map((turn) => turn.username).filter((name): name is string => name !== null),
      gap: now.length === 0 || now.some((turn) => turn.problem !== null),
    };
  });
  // The person's own turns first, then by name.
  return rows.sort((a, b) => Number(b.me) - Number(a.me) || a.name.localeCompare(b.name));
}
