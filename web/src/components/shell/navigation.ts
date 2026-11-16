import type { PlainKey } from "@/i18n";
import type { Me, SidebarPreferences } from "@/lib/api";
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
  /** Names the item in the person's sidebar preferences (ADR-0137): never change one. */
  id: string;
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
      { id: "fleet", label: "shell.nav.fleet", href: "/", icon: "fleet", alsoCurrent: ["/servers/"] },
      { id: "dashboards", label: "shell.nav.dashboards", href: DASHBOARDS_PATH, icon: "dashboards" },
      { id: "incidents", label: "shell.nav.incidents", href: "/incidents/", icon: "incidents", incidentCount: true },
      { id: "silences", label: "shell.nav.silences", href: SILENCES_PATH, icon: "bell" },
      { id: "onCall", label: "shell.nav.onCall", href: ON_CALL_PATH, icon: "users" },
    ],
  },
  {
    heading: "shell.nav.updates",
    items: [
      { id: "mirrors", label: "shell.nav.mirrors", href: MIRRORS_PATH, icon: "mirrors", visible: administersEverything },
      { id: "automaticUpdates", label: "shell.nav.automaticUpdates", href: AUTO_UPDATE_PATH, icon: "campaigns", visible: administersEverything },
      // The handoff moves it to /updates/campaigns/ with its redesign.
      { id: "campaigns", label: "shell.nav.campaigns", href: CAMPAIGNS_PATH, icon: "campaigns" },
    ],
    visible: readsCustomers,
  },
  {
    heading: "shell.nav.remediation",
    items: [
      { id: "approvals", label: "shell.nav.approvals", href: APPROVALS_PATH, icon: "approvals" },
      { id: "audit", label: "shell.nav.audit", href: AUDIT_PATH, icon: "audit" },
    ],
  },
];

/** The one way to the administration pages, pinned at the bottom of the sidebar. */
export const SETTINGS_ITEM: NavItem = {
  id: "settings",
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

/**
 * `items` in the person's `order`: the items it lists take the places of
 * listed items, in its order; the others keep their default place.
 */
export function ordered<T extends { id: string }>(items: T[], order: readonly string[] = []): T[] {
  const listed = items.filter((item) => order.includes(item.id)).sort((a, b) => order.indexOf(a.id) - order.indexOf(b.id));
  let next = 0;
  return items.map((item) => (order.includes(item.id) ? listed[next++]! : item));
}

/** The sections `me` sees in the person's order, hidden items included, for the sidebar's editor. */
export function orderedNavigation(me: Me, sidebar: SidebarPreferences = {}): NavSection[] {
  return visibleNavigation(me).map((section) => ({ ...section, items: ordered(section.items, sidebar.order) }));
}

/**
 * The sidebar as the person arranged it (ADR-0137), within what `me`'s
 * roles show: a preference never shows an item the roles hide. Sections
 * left empty are dropped. Settings is not here: it always shows.
 */
export function arrangedNavigation(me: Me, sidebar: SidebarPreferences = {}): NavSection[] {
  const hidden = sidebar.hidden ?? [];
  return orderedNavigation(me, sidebar)
    .map((section) => ({ ...section, items: section.items.filter((item) => !hidden.includes(item.id)) }))
    .filter((section) => section.items.length > 0);
}
