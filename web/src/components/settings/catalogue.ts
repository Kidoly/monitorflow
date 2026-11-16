import { type PlainKey, t } from "@/i18n";
import type { Me } from "@/lib/api";
import { administersEverything, isAdministrator, multiCustomer } from "@/lib/me";
import {
  AI_SETTINGS_PATH,
  API_KEYS_PATH,
  BACKUPS_PATH,
  CERTIFICATE_AUTHORITY_PATH,
  GENERAL_SETTINGS_PATH,
  INTEGRATIONS_PATH,
  LICENSE_PATH,
  NOTIFICATIONS_PATH,
  PLUGINS_PATH,
  RELAYS_PATH,
  SERVER_UPDATE_PATH,
  SETTINGS_PATH,
  SITES_PATH,
  TOKENS_PATH,
  USERS_PATH,
} from "@/lib/routes";

import type { IconName } from "../shell/icons";

export type SettingsId =
  | "sites"
  | "relays"
  | "add-servers"
  | "certificate-authority"
  | "notifications"
  | "integrations"
  | "users"
  | "api-keys"
  | "ai"
  | "plugins"
  | "general"
  | "server-update"
  | "backups"
  | "license";

export interface SettingsEntry {
  id: SettingsId;
  label: PlainKey;
  /** One line under the name; `me` changes it where the words depend on what they see. */
  description: (me: Me) => string;
  href: string;
  icon: IconName;
  /** More words the search finds it by, not shown. */
  keywords?: PlainKey;
  /** UX only, like the sidebar's: the API refuses what the roles do not allow. */
  visible?: (me: Me) => boolean;
}

export interface SettingsGroup {
  heading: PlainKey;
  entries: SettingsEntry[];
}

const text = (key: PlainKey) => () => t(key);

/** Every settings page, in groups, in the order the Settings page and its sidebar show them. */
export const SETTINGS_GROUPS: SettingsGroup[] = [
  {
    heading: "settings.group.fleet",
    entries: [
      {
        id: "sites",
        label: "settings.sites.label",
        description: (me) =>
          multiCustomer(me) ? t("settings.sites.descriptionCustomers") : t("settings.sites.description"),
        href: SITES_PATH,
        icon: "sites",
        keywords: "settings.sites.keywords",
      },
      {
        id: "relays",
        label: "settings.relays.label",
        description: text("settings.relays.description"),
        href: RELAYS_PATH,
        icon: "relays",
      },
      {
        id: "add-servers",
        label: "settings.addServers.label",
        description: text("settings.addServers.description"),
        href: TOKENS_PATH,
        icon: "tokens",
        keywords: "settings.addServers.keywords",
      },
      {
        id: "certificate-authority",
        label: "settings.certificateAuthority.label",
        description: text("settings.certificateAuthority.description"),
        href: CERTIFICATE_AUTHORITY_PATH,
        icon: "authority",
        keywords: "settings.certificateAuthority.keywords",
        visible: administersEverything,
      },
    ],
  },
  {
    heading: "settings.group.alerting",
    entries: [
      {
        id: "notifications",
        label: "settings.notifications.label",
        description: text("settings.notifications.description"),
        href: NOTIFICATIONS_PATH,
        icon: "bell",
        keywords: "settings.notifications.keywords",
      },
      {
        id: "integrations",
        label: "settings.integrations.label",
        description: text("settings.integrations.description"),
        href: INTEGRATIONS_PATH,
        icon: "integrations",
        keywords: "settings.integrations.keywords",
      },
    ],
  },
  {
    heading: "settings.group.access",
    entries: [
      {
        id: "users",
        label: "settings.users.label",
        description: text("settings.users.description"),
        href: USERS_PATH,
        icon: "users",
        keywords: "settings.users.keywords",
      },
      {
        id: "api-keys",
        label: "settings.apiKeys.label",
        description: text("settings.apiKeys.description"),
        href: API_KEYS_PATH,
        icon: "tokens",
        keywords: "settings.apiKeys.keywords",
      },
    ],
  },
  {
    heading: "settings.group.extensions",
    entries: [
      {
        id: "ai",
        label: "settings.ai.label",
        description: text("settings.ai.description"),
        href: AI_SETTINGS_PATH,
        icon: "ai",
        keywords: "settings.ai.keywords",
        visible: administersEverything,
      },
      {
        id: "plugins",
        label: "settings.plugins.label",
        description: text("settings.plugins.description"),
        href: PLUGINS_PATH,
        icon: "plugin",
        keywords: "settings.plugins.keywords",
        visible: administersEverything,
      },
    ],
  },
  {
    heading: "settings.group.system",
    entries: [
      {
        id: "general",
        label: "settings.general.label",
        description: text("settings.general.description"),
        href: GENERAL_SETTINGS_PATH,
        icon: "settings",
        keywords: "settings.general.keywords",
        visible: administersEverything,
      },
      {
        id: "server-update",
        label: "settings.serverUpdate.label",
        description: text("settings.serverUpdate.description"),
        href: SERVER_UPDATE_PATH,
        icon: "update",
        keywords: "settings.serverUpdate.keywords",
        visible: administersEverything,
      },
      {
        id: "backups",
        label: "settings.backups.label",
        description: text("settings.backups.description"),
        href: BACKUPS_PATH,
        icon: "backups",
        keywords: "settings.backups.keywords",
        visible: administersEverything,
      },
      {
        id: "license",
        label: "settings.license.label",
        description: text("settings.license.description"),
        href: LICENSE_PATH,
        icon: "license",
        visible: administersEverything,
      },
    ],
  },
];

/** Whether `me` sees Settings at all: administrators, as the Administration section was. UX only. */
export const seesSettings = (me: Me): boolean => isAdministrator(me);

/** The groups and entries `me` sees, empty groups dropped. */
export function visibleSettings(me: Me): SettingsGroup[] {
  if (!seesSettings(me)) return [];
  return SETTINGS_GROUPS.map((group) => ({
    ...group,
    entries: group.entries.filter((entry) => entry.visible?.(me) ?? true),
  })).filter((group) => group.entries.length > 0);
}

/**
 * The entries whose name, description or keywords, in the active language,
 * hold every word of `query`, ignoring case; a group whose heading matches keeps all of its
 * entries. Empty groups are dropped.
 */
export function filterSettings(groups: SettingsGroup[], query: string, me: Me): SettingsGroup[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return groups;
  return groups
    .map((group) => {
      if (words.every((w) => t(group.heading).toLowerCase().includes(w))) return group;
      const entries = group.entries.filter((entry) => {
        const keywords = entry.keywords ? t(entry.keywords) : "";
        const haystack = `${t(entry.label)} ${entry.description(me)} ${keywords}`.toLowerCase();
        return words.every((w) => haystack.includes(w));
      });
      return { ...group, entries };
    })
    .filter((group) => group.entries.length > 0);
}

/** The settings entry `pathname` is on or under, if any. */
export function currentSettingsEntry(pathname: string): SettingsEntry | undefined {
  return SETTINGS_GROUPS.flatMap((g) => g.entries).find((entry) => pathname.startsWith(entry.href));
}

/** Whether `pathname` is a settings page, not the Settings list itself. */
export const insideSettings = (pathname: string): boolean =>
  pathname.startsWith(SETTINGS_PATH) && pathname !== SETTINGS_PATH && pathname !== SETTINGS_PATH.slice(0, -1);
