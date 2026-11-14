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
  label: string;
  /** One line under the name; `me` changes it where the words depend on what they see. */
  description: (me: Me) => string;
  href: string;
  icon: IconName;
  /** More words the search finds it by, not shown. */
  keywords?: string;
  /** UX only, like the sidebar's: the API refuses what the roles do not allow. */
  visible?: (me: Me) => boolean;
}

export interface SettingsGroup {
  heading: string;
  entries: SettingsEntry[];
}

const text = (line: string) => () => line;

/** Every settings page, in groups, in the order the Settings page and its sidebar show them. */
export const SETTINGS_GROUPS: SettingsGroup[] = [
  {
    heading: "Fleet",
    entries: [
      {
        id: "sites",
        label: "Sites",
        description: (me) =>
          multiCustomer(me) ? "Customers and the sites their servers belong to." : "The places your servers are in.",
        href: SITES_PATH,
        icon: "sites",
        keywords: "customers offline alerts",
      },
      {
        id: "relays",
        label: "Relays",
        description: text("Carry the servers of a site that cannot reach this server."),
        href: RELAYS_PATH,
        icon: "relays",
      },
      {
        id: "add-servers",
        label: "Add servers",
        description: text("Install commands, single-use tokens and where agents install from."),
        href: TOKENS_PATH,
        icon: "tokens",
        keywords: "install command token install source approval",
      },
      {
        id: "certificate-authority",
        label: "Certificate authority",
        description: text("Gives every server and relay its certificate and signs commands."),
        href: CERTIFICATE_AUTHORITY_PATH,
        icon: "authority",
        keywords: "certificates change",
        visible: administersEverything,
      },
    ],
  },
  {
    heading: "Alerting",
    entries: [
      {
        id: "notifications",
        label: "Notifications",
        description: text("Email, Slack, Teams, Discord and webhooks for incidents, approvals and failed updates."),
        href: NOTIFICATIONS_PATH,
        icon: "bell",
        keywords: "channels follow-up",
      },
      {
        id: "integrations",
        label: "Integrations",
        description: text("Signed events for a PSA or your own automation."),
        href: INTEGRATIONS_PATH,
        icon: "integrations",
        keywords: "events signing secret",
      },
    ],
  },
  {
    heading: "Access",
    entries: [
      {
        id: "users",
        label: "Users and roles",
        description: text("Who signs in, and what they can do."),
        href: USERS_PATH,
        icon: "users",
        keywords: "people invite password",
      },
      {
        id: "api-keys",
        label: "API keys",
        description: text("Keys for scripts and other tools, and the API documentation."),
        href: API_KEYS_PATH,
        icon: "tokens",
        keywords: "public api documentation",
      },
    ],
  },
  {
    heading: "AI & extensions",
    entries: [
      {
        id: "ai",
        label: "AI",
        description: text("An API key and a model to diagnose incidents and propose fixes."),
        href: AI_SETTINGS_PATH,
        icon: "ai",
        keywords: "provider model local",
        visible: administersEverything,
      },
      {
        id: "plugins",
        label: "Plugins",
        description: text("Checks and notification channels someone else wrote, and the publishers you trust."),
        href: PLUGINS_PATH,
        icon: "plugin",
        keywords: "publisher",
        visible: administersEverything,
      },
    ],
  },
  {
    heading: "System",
    entries: [
      {
        id: "general",
        label: "General",
        description: text("Server address, sign-in, package address and data retention."),
        href: GENERAL_SETTINGS_PATH,
        icon: "settings",
        keywords: "session retention address mirror",
        visible: administersEverything,
      },
      {
        id: "server-update",
        label: "Server update",
        description: text("Update this server, or import a release without Internet."),
        href: SERVER_UPDATE_PATH,
        icon: "update",
        keywords: "version release",
        visible: administersEverything,
      },
      {
        id: "backups",
        label: "Backups",
        description: text("Daily backups, Back up now, and downloads."),
        href: BACKUPS_PATH,
        icon: "backups",
        keywords: "restore",
        visible: administersEverything,
      },
      {
        id: "license",
        label: "License",
        description: text("The license key, for more than one central server."),
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
 * The entries whose name, description or keywords hold every word of
 * `query`, ignoring case; a group whose heading matches keeps all of its
 * entries. Empty groups are dropped.
 */
export function filterSettings(groups: SettingsGroup[], query: string, me: Me): SettingsGroup[] {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (words.length === 0) return groups;
  return groups
    .map((group) => {
      if (words.every((w) => group.heading.toLowerCase().includes(w))) return group;
      const entries = group.entries.filter((entry) => {
        const haystack = `${entry.label} ${entry.description(me)} ${entry.keywords ?? ""}`.toLowerCase();
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
