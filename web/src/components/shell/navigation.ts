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
  label: string;
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
  heading: string;
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
    heading: "Monitoring",
    items: [
      { label: "Fleet", href: "/", icon: "fleet", alsoCurrent: ["/servers/"] },
      { label: "Dashboards", href: DASHBOARDS_PATH, icon: "dashboards" },
      { label: "Incidents", href: "/incidents/", icon: "incidents", incidentCount: true },
      { label: "Silences", href: SILENCES_PATH, icon: "bell" },
      { label: "On call", href: ON_CALL_PATH, icon: "users" },
    ],
  },
  {
    heading: "Updates",
    items: [
      { label: "Mirrors", href: MIRRORS_PATH, icon: "mirrors", visible: administersEverything },
      { label: "Automatic updates", href: AUTO_UPDATE_PATH, icon: "campaigns", visible: administersEverything },
      // The handoff moves it to /updates/campaigns/ with its redesign.
      { label: "Campaigns", href: CAMPAIGNS_PATH, icon: "campaigns" },
    ],
    visible: readsCustomers,
  },
  {
    heading: "Remediation",
    items: [
      { label: "Approvals", href: APPROVALS_PATH, icon: "approvals" },
      { label: "Audit log", href: AUDIT_PATH, icon: "audit" },
    ],
  },
];

/** The one way to the administration pages, pinned at the bottom of the sidebar. */
export const SETTINGS_ITEM: NavItem = {
  label: "Settings",
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
