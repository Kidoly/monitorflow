import type { PlainKey } from "@/i18n";
import type { Me } from "@/lib/api";
import { administersEverything, isAdministrator, readsCustomers } from "@/lib/me";
import {
  APPROVALS_PATH,
  AUDIT_PATH,
  AUTO_UPDATE_PATH,
  CAMPAIGNS_PATH,
  DASHBOARDS_PATH,
  MIRRORS_PATH,
  ON_CALL_PATH,
  SETTINGS_PATH,
  SILENCES_PATH,
} from "@/lib/routes";

import type { IconName } from "./icons";

export interface NavItem {
  label: PlainKey;
  /** Null while the page does not exist yet: the item is hidden. */
  href: string | null;
  icon: IconName;
  /** Extra path prefixes that keep the item current, like server pages under Fleet. */
  alsoCurrent?: string[];
  /** Shows the open incidents count. */
  incidentCount?: boolean;
  /** UX only, like the section's: hides the item from roles the API refuses. */
  visible?: (me: Me) => boolean;
}

export interface NavSection {
  heading: PlainKey;
  items: NavItem[];
  /** UX only: the API refuses what the roles do not allow. */
  visible?: (me: Me) => boolean;
}

/**
 * The daily work of the handoff's route table, in its order. Routes the web
 * interface does not have yet are null and hidden until their page lands.
 * Administration pages are under Settings, pinned at the bottom (ADR-0133).
 */
export const NAVIGATION: NavSection[] = [
  {
    heading: "shell.nav.monitoring",
    items: [
      { label: "shell.nav.fleet", href: "/", icon: "fleet", alsoCurrent: ["/servers/"] },
      { label: "shell.nav.dashboards", href: DASHBOARDS_PATH, icon: "dashboards" },
      { label: "shell.nav.incidents", href: "/incidents/", icon: "incidents", incidentCount: true },
      { label: "shell.nav.silences", href: SILENCES_PATH, icon: "bell" },
      { label: "shell.nav.onCall", href: ON_CALL_PATH, icon: "users" },
    ],
  },
  {
    heading: "shell.nav.updates",
    items: [
      { label: "shell.nav.mirrors", href: MIRRORS_PATH, icon: "mirrors", visible: administersEverything },
      { label: "shell.nav.automaticUpdates", href: AUTO_UPDATE_PATH, icon: "campaigns", visible: administersEverything },
      // The handoff moves it to /updates/campaigns/ with its redesign.
      { label: "shell.nav.campaigns", href: CAMPAIGNS_PATH, icon: "campaigns" },
    ],
    visible: readsCustomers,
  },
  {
    heading: "shell.nav.remediation",
    items: [
      { label: "shell.nav.approvals", href: APPROVALS_PATH, icon: "approvals" },
      { label: "shell.nav.audit", href: AUDIT_PATH, icon: "audit" },
    ],
  },
];

/** The one way to the administration pages, pinned at the bottom of the sidebar. */
export const SETTINGS_ITEM: NavItem = {
  label: "shell.nav.settings",
  href: SETTINGS_PATH,
  icon: "settings",
  // Everything under /settings/ swaps the sidebar for the settings list; the list itself keeps this one.
  visible: isAdministrator,
};

/** Whether `item` is the current page's: its path, or a page under it. */
export function isCurrent(item: NavItem, pathname: string): boolean {
  if (!item.href) return false;
  const prefixes = [item.href, ...(item.alsoCurrent ?? [])];
  return prefixes.some((prefix) => (prefix === "/" ? pathname === "/" : pathname.startsWith(prefix)));
}

/** The sections and items `me` sees, empty sections dropped. */
export function visibleNavigation(me: Me): NavSection[] {
  return NAVIGATION.filter((section) => section.visible?.(me) ?? true)
    .map((section) => ({
      ...section,
      items: section.items.filter((item) => item.href !== null && (item.visible?.(me) ?? true)),
    }))
    .filter((section) => section.items.length > 0);
}
