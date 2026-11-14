import type {
  AiSettings,
  ApiKey,
  BackupsView,
  CertificateAuthorityView,
  InstallKeyList,
  IntegrationList,
  LicenseView,
  NotificationChannel,
  Relay,
  ServerUpdateView,
  Site,
  UserAccount,
} from "@/lib/api";
import { authorityStatus } from "@/lib/certificateAuthority";
import { formatDay as formatLicenseDay } from "@/lib/license";
import { inProgress } from "@/lib/serverUpdate";
import { formatDay } from "@/components/ui/time";
import type { Tone } from "@/components/ui/tone";

import type { SettingsId } from "./catalogue";

/**
 * One short line of where a settings page stands, from what the web
 * interface already reads: "2 relays connected", "Last backup 03:00 today,
 * OK". `warn` and `bad` need someone, and put a badge on Settings.
 */
export interface SettingsStatus {
  text: string;
  tone: Tone;
}

export type SettingsStatuses = Partial<Record<SettingsId, SettingsStatus>>;

/** Whether a status asks for someone. */
export const needsAttention = (status: SettingsStatus | undefined): boolean =>
  status?.tone === "warn" || status?.tone === "bad";

/** The worst tone among statuses that need someone, or null. */
export function attention(statuses: SettingsStatuses): { count: number; tone: "warn" | "bad" } | null {
  const pressing = Object.values(statuses).filter(needsAttention);
  if (pressing.length === 0) return null;
  return { count: pressing.length, tone: pressing.some((s) => s.tone === "bad") ? "bad" : "warn" };
}

const count = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** "3 sites", or "2 customers · 5 sites" for someone who sees several customers (`multiCustomer`). */
export function sitesStatus(sites: Site[], multiCustomer: boolean): SettingsStatus {
  const text = count(sites.length, "site", "sites");
  if (!multiCustomer) return { text, tone: "neutral" };
  const customers = new Set(sites.map((s) => s.customer_id)).size;
  return { text: `${count(customers, "customer", "customers")} · ${text}`, tone: "neutral" };
}

/** Relays waiting for confirmation first, then those offline, then "3 relays connected". */
export function relaysStatus(relays: Relay[]): SettingsStatus | undefined {
  const current = relays.filter((r) => !r.revoked_at);
  if (current.length === 0) return undefined;
  const waiting = current.filter((r) => r.pending_names).length;
  if (waiting > 0) return { text: `${count(waiting, "relay waits", "relays wait")} for confirmation`, tone: "warn" };
  const online = current.filter((r) => r.status === "online").length;
  if (online < current.length) return { text: `${online} of ${count(current.length, "relay", "relays")} connected`, tone: "warn" };
  return { text: `${count(online, "relay", "relays")} connected`, tone: "ok" };
}

/** Servers waiting for approval first, then the install commands that work. */
export function addServersStatus(list: InstallKeyList): SettingsStatus {
  if (list.pending_hosts.length > 0) {
    return { text: `${count(list.pending_hosts.length, "server waits", "servers wait")} for approval`, tone: "warn" };
  }
  const active = list.keys.filter((k) => k.state === "active").length;
  return { text: active === 0 ? "No install command in use" : `${count(active, "install command", "install commands")} in use`, tone: "neutral" };
}

/** The authority page's headline; machines that need a new install come first. */
export function authorityLine(view: CertificateAuthorityView): SettingsStatus {
  if (view.cut_off.length > 0) {
    return { text: `${count(view.cut_off.length, "machine needs", "machines need")} a new install`, tone: "bad" };
  }
  const [text, tone] = authorityStatus(view);
  return { text, tone };
}

/** Channels whose last delivery failed, else how many there are. */
export function notificationsStatus(channels: NotificationChannel[]): SettingsStatus {
  const failing = channels.filter((c) => c.last_error).length;
  if (failing > 0) return { text: `${count(failing, "channel fails", "channels fail")} to deliver`, tone: "warn" };
  return { text: channels.length === 0 ? "No channel yet" : count(channels.length, "channel", "channels"), tone: "neutral" };
}

/** Turned-off integrations, then failing ones, else how many there are. */
export function integrationsStatus(list: IntegrationList): SettingsStatus | undefined {
  const { endpoints } = list;
  if (endpoints.length === 0) return undefined;
  const off = endpoints.filter((e) => e.state === "disabled").length;
  if (off > 0) return { text: `${count(off, "integration", "integrations")} turned off after failing`, tone: "bad" };
  const failing = endpoints.filter((e) => e.failing_since).length;
  if (failing > 0) return { text: `${count(failing, "integration fails", "integrations fail")} to deliver`, tone: "warn" };
  return { text: count(endpoints.length, "integration", "integrations"), tone: "neutral" };
}

/** "5 users · 1 invited". */
export function usersStatus(users: UserAccount[]): SettingsStatus {
  const active = users.filter((u) => u.state === "active").length;
  const invited = users.filter((u) => u.state === "invited").length;
  return { text: [count(active, "user", "users"), invited > 0 && `${invited} invited`].filter(Boolean).join(" · "), tone: "neutral" };
}

/** Keys that still work. */
export function apiKeysStatus(keys: ApiKey[], now: Date): SettingsStatus {
  const working = keys.filter((k) => k.expires_at === null || Date.parse(k.expires_at) > now.getTime()).length;
  return { text: working === 0 ? "No API key" : count(working, "API key", "API keys"), tone: "neutral" };
}

/** "On · <model>", or "Off". */
export function aiStatus(ai: AiSettings): SettingsStatus {
  return ai.enabled ? { text: `On · ${ai.model}`, tone: "ok" } : { text: "Off", tone: "neutral" };
}

/** Installed plugins, and those turned off. */
export function pluginsStatus(plugins: { enabled: boolean }[]): SettingsStatus {
  if (plugins.length === 0) return { text: "None installed", tone: "neutral" };
  const off = plugins.filter((p) => !p.enabled).length;
  return { text: [`${plugins.length} installed`, off > 0 && `${off} turned off`].filter(Boolean).join(" · "), tone: "neutral" };
}

/** A failed update, one under way, a newer release, or the version it runs. */
export function serverUpdateStatus(view: ServerUpdateView): SettingsStatus {
  const run = view.update;
  if (inProgress(run, view.requested)) return { text: `Updating${run ? ` to ${run.to}` : ""}…`, tone: "info" };
  if (run && (run.state === "failed" || run.state === "rolled_back")) return { text: `The update to ${run.to} failed`, tone: "bad" };
  if (view.available) return { text: `${view.available.version} available`, tone: "warn" };
  return { text: `Up to date, ${view.running_version}`, tone: "ok" };
}

/** A daily backup older than this is late. */
const LATE_MS = 36 * 60 * 60 * 1000;

/** "Last backup 03:00 today, OK", or what went wrong. */
export function backupsStatus(view: BackupsView, now: Date): SettingsStatus {
  if (!view.enabled) return { text: "Not set up on this server", tone: "neutral" };
  if (view.last?.state === "running") return { text: "Backing up…", tone: "info" };
  if (view.last?.state === "failed") return { text: "The last backup failed", tone: "bad" };
  if (!view.last_success_at) return { text: "No backup yet", tone: "warn" };
  const at = new Date(view.last_success_at);
  const late = now.getTime() - at.getTime() > LATE_MS;
  return { text: `Last backup ${formatDay(at, now)}${late ? "" : ", OK"}`, tone: late ? "warn" : "ok" };
}

/**
 * Where the license key stands. Without central servers added, an expired
 * key changes nothing, so it does not ask for anyone.
 */
export function licenseStatus(view: LicenseView): SettingsStatus | undefined {
  if (!view.available) return undefined;
  const added = (view.added?.length ?? 0) > 0;
  const expires = view.license ? formatLicenseDay(view.license.expires) : null;
  switch (view.state) {
    case "valid":
      return { text: expires ? `Valid until ${expires}` : "Valid", tone: "ok" };
    case "expires_soon":
      return { text: expires ? `Expires on ${expires}` : "Expires soon", tone: added ? "warn" : "neutral" };
    case "expired":
      return { text: expires ? `Expired on ${expires}` : "Expired", tone: added ? "bad" : "neutral" };
    case "invalid":
      return { text: "The key no longer verifies", tone: added ? "bad" : "neutral" };
    default:
      return { text: "No license key", tone: added ? "bad" : "neutral" };
  }
}
