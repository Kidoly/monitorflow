import type { Me } from "@/lib/api";
import { isAdministrator, readsCustomers } from "@/lib/me";
import { CAMPAIGNS_PATH, RELAYS_PATH, SITES_PATH, TOKENS_PATH, USERS_PATH } from "@/lib/routes";

import type { IconName } from "./icons";

export interface NavItem {
  label: string;
  /** Null while the page does not exist yet: the item is hidden. */
  href: string | null;
  icon: IconName;
  /** Only the collapsed sidebar draws the icon (the handoff's Administration items). */
  iconWhenCollapsed?: boolean;
  /** Extra path prefixes that keep the item current, like server pages under Fleet. */
  alsoCurrent?: string[];
  /** Shows the open incidents count. */
  incidentCount?: boolean;
}

export interface NavSection {
  heading: string;
  items: NavItem[];
  /** UX only: the API refuses what the roles do not allow. */
  visible?: (me: Me) => boolean;
}

/**
 * The full navigation of the handoff's route table, in its order. Routes the
 * web interface does not have yet are null and hidden until their page lands.
 */
export const NAVIGATION: NavSection[] = [
  {
    heading: "Monitoring",
    items: [
      { label: "Fleet", href: "/", icon: "fleet", alsoCurrent: ["/servers/"] },
      { label: "Incidents", href: "/incidents/", icon: "incidents", incidentCount: true },
    ],
  },
  {
    heading: "Updates",
    items: [
      { label: "Mirrors", href: null, icon: "mirrors" },
      // The handoff moves it to /updates/campaigns/ with its redesign.
      { label: "Campaigns", href: CAMPAIGNS_PATH, icon: "campaigns" },
    ],
    visible: readsCustomers,
  },
  {
    heading: "Remediation",
    items: [
      { label: "Approvals", href: null, icon: "approvals" },
      { label: "Audit log", href: null, icon: "audit" },
    ],
  },
  {
    heading: "Administration",
    items: [
      { label: "Relays", href: RELAYS_PATH, icon: "relays", iconWhenCollapsed: true },
      { label: "Sites", href: SITES_PATH, icon: "sites", iconWhenCollapsed: true },
      { label: "Enrollment tokens", href: TOKENS_PATH, icon: "tokens", iconWhenCollapsed: true },
      { label: "Users and roles", href: USERS_PATH, icon: "users", iconWhenCollapsed: true },
    ],
    visible: isAdministrator,
  },
];

/** Whether `item` is the current page's: its path, or a page under it. */
export function isCurrent(item: NavItem, pathname: string): boolean {
  if (!item.href) return false;
  const prefixes = [item.href, ...(item.alsoCurrent ?? [])];
  return prefixes.some((prefix) => (prefix === "/" ? pathname === "/" : pathname.startsWith(prefix)));
}

/** The sections and items `me` sees, empty sections dropped. */
export function visibleNavigation(me: Me): NavSection[] {
  return NAVIGATION.filter((section) => section.visible?.(me) ?? true)
    .map((section) => ({ ...section, items: section.items.filter((item) => item.href !== null) }))
    .filter((section) => section.items.length > 0);
}
