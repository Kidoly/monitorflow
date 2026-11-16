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
import { t } from "@/i18n";
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

/** "3 sites", or "2 customers · 5 sites" for someone who sees several customers (`multiCustomer`). */
export function sitesStatus(sites: Site[], multiCustomer: boolean): SettingsStatus {
  const text = t("settings.status.sites", { count: sites.length });
  if (!multiCustomer) return { text, tone: "neutral" };
  const customers = new Set(sites.map((s) => s.customer_id)).size;
  return { text: `${t("settings.status.customers", { count: customers })} · ${text}`, tone: "neutral" };
}

/** Relays waiting for confirmation first, then those offline, then "3 relays connected". */
export function relaysStatus(relays: Relay[]): SettingsStatus | undefined {
  const current = relays.filter((r) => !r.revoked_at);
  if (current.length === 0) return undefined;
  const waiting = current.filter((r) => r.pending_names).length;
  if (waiting > 0) return { text: t("settings.status.relaysWaiting", { count: waiting }), tone: "warn" };
  const online = current.filter((r) => r.status === "online").length;
  if (online < current.length) {
    return { text: t("settings.status.relaysSomeConnected", { online, count: current.length }), tone: "warn" };
  }
  return { text: t("settings.status.relaysConnected", { count: online }), tone: "ok" };
}

/** Servers waiting for approval first, then the install commands that work. */
export function addServersStatus(list: InstallKeyList): SettingsStatus {
  if (list.pending_hosts.length > 0) {
    return { text: t("settings.status.serversWaiting", { count: list.pending_hosts.length }), tone: "warn" };
  }
  const active = list.keys.filter((k) => k.state === "active").length;
  return {
    text: active === 0 ? t("settings.status.noInstallCommand") : t("settings.status.installCommands", { count: active }),
    tone: "neutral",
  };
}

/** The authority page's headline; machines that need a new install come first. */
export function authorityLine(view: CertificateAuthorityView): SettingsStatus {
  if (view.cut_off.length > 0) {
    return { text: t("settings.status.newInstall", { count: view.cut_off.length }), tone: "bad" };
  }
  const [text, tone] = authorityStatus(view);
  return { text, tone };
}

/** Channels whose last delivery failed, else how many there are. */
export function notificationsStatus(channels: NotificationChannel[]): SettingsStatus {
  const failing = channels.filter((c) => c.last_error).length;
  if (failing > 0) return { text: t("settings.status.channelsFailing", { count: failing }), tone: "warn" };
  return {
    text: channels.length === 0 ? t("settings.status.noChannel") : t("settings.status.channels", { count: channels.length }),
    tone: "neutral",
  };
}

/** Turned-off integrations, then failing ones, else how many there are. */
export function integrationsStatus(list: IntegrationList): SettingsStatus | undefined {
  const { endpoints } = list;
  if (endpoints.length === 0) return undefined;
  const off = endpoints.filter((e) => e.state === "disabled").length;
  if (off > 0) return { text: t("settings.status.integrationsOff", { count: off }), tone: "bad" };
  const failing = endpoints.filter((e) => e.failing_since).length;
  if (failing > 0) return { text: t("settings.status.integrationsFailing", { count: failing }), tone: "warn" };
  return { text: t("settings.status.integrations", { count: endpoints.length }), tone: "neutral" };
}

/** "5 users · 1 invited". */
export function usersStatus(users: UserAccount[]): SettingsStatus {
  const active = users.filter((u) => u.state === "active").length;
  const invited = users.filter((u) => u.state === "invited").length;
  const parts = [t("settings.status.users", { count: active }), invited > 0 && t("settings.status.invited", { count: invited })];
  return { text: parts.filter(Boolean).join(" · "), tone: "neutral" };
}

/** Keys that still work. */
export function apiKeysStatus(keys: ApiKey[], now: Date): SettingsStatus {
  const working = keys.filter((k) => k.expires_at === null || Date.parse(k.expires_at) > now.getTime()).length;
  return { text: working === 0 ? t("settings.status.noApiKey") : t("settings.status.apiKeys", { count: working }), tone: "neutral" };
}

/** "On · <model>", or "Off". */
export function aiStatus(ai: AiSettings): SettingsStatus {
  return ai.enabled
    ? { text: t("settings.status.aiOn", { model: ai.model }), tone: "ok" }
    : { text: t("settings.status.aiOff"), tone: "neutral" };
}

/** Installed plugins, and those turned off. */
export function pluginsStatus(plugins: { enabled: boolean }[]): SettingsStatus {
  if (plugins.length === 0) return { text: t("settings.status.noPlugin"), tone: "neutral" };
  const off = plugins.filter((p) => !p.enabled).length;
  const parts = [
    t("settings.status.pluginsInstalled", { count: plugins.length }),
    off > 0 && t("settings.status.pluginsOff", { count: off }),
  ];
  return { text: parts.filter(Boolean).join(" · "), tone: "neutral" };
}

/** A failed update, one under way, a newer release, or the version it runs. */
export function serverUpdateStatus(view: ServerUpdateView): SettingsStatus {
  const run = view.update;
  if (inProgress(run, view.requested)) {
    return { text: run ? t("settings.status.updatingTo", { version: run.to }) : t("settings.status.updating"), tone: "info" };
  }
  if (run && (run.state === "failed" || run.state === "rolled_back")) {
    return { text: t("settings.status.updateFailed", { version: run.to }), tone: "bad" };
  }
  if (view.available) return { text: t("settings.status.updateAvailable", { version: view.available.version }), tone: "warn" };
  return { text: t("settings.status.upToDate", { version: view.running_version }), tone: "ok" };
}

/** A daily backup older than this is late. */
const LATE_MS = 36 * 60 * 60 * 1000;

/** "Last backup 03:00 today, OK", or what went wrong. */
export function backupsStatus(view: BackupsView, now: Date): SettingsStatus {
  if (!view.enabled) return { text: t("settings.status.backupsOff"), tone: "neutral" };
  if (view.last?.state === "running") return { text: t("settings.status.backingUp"), tone: "info" };
  if (view.last?.state === "failed") return { text: t("settings.status.backupFailed"), tone: "bad" };
  if (!view.last_success_at) return { text: t("settings.status.noBackup"), tone: "warn" };
  const at = new Date(view.last_success_at);
  const late = now.getTime() - at.getTime() > LATE_MS;
  const day = formatDay(at, now);
  return late
    ? { text: t("settings.status.lastBackupLate", { day }), tone: "warn" }
    : { text: t("settings.status.lastBackup", { day }), tone: "ok" };
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
      return { text: expires ? t("settings.status.validUntil", { day: expires }) : t("settings.status.valid"), tone: "ok" };
    case "expires_soon":
      return {
        text: expires ? t("settings.status.expiresOn", { day: expires }) : t("settings.status.expiresSoon"),
        tone: added ? "warn" : "neutral",
      };
    case "expired":
      return {
        text: expires ? t("settings.status.expiredOn", { day: expires }) : t("settings.status.expired"),
        tone: added ? "bad" : "neutral",
      };
    case "invalid":
      return { text: t("settings.status.keyInvalid"), tone: added ? "bad" : "neutral" };
    default:
      return { text: t("settings.status.noLicense"), tone: added ? "bad" : "neutral" };
  }
}
