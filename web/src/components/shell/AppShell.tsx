"use client";

import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import { Fragment, type ReactNode, useCallback, useEffect, useState } from "react";

import { Button } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { Label } from "@/components/ui/StatusLabel";
import { t } from "@/i18n";
import { type Customer, type Me, type Site, api, isAbortError } from "@/lib/api";
import { administersEverything, multiCustomer, roleNames } from "@/lib/me";
import { setPreference, usePreferences } from "@/lib/preferences";
import { type Scope, scopedHref, useScope } from "@/lib/scope";
import { PREFERENCES_PATH } from "@/lib/routes";
import { subscribeTheme } from "@/lib/theme";
import { useMediaQuery } from "@/lib/useMediaQuery";
import { usePolledData } from "@/lib/usePolledData";

import { insideSettings } from "../settings/catalogue";
import { attention } from "../settings/status";
import { useSettingsStatuses } from "../settings/SettingsStatus";

import { Icon } from "./icons";
import { SETTINGS_ITEM, arrangedNavigation, isCurrent } from "./navigation";
import { SettingsSidebar } from "./SettingsSidebar";
import styles from "./Shell.module.css";
import { ScopeSwitcher } from "./ScopeSwitcher";
import { StatusStrip } from "./StatusStrip";
import { CertificateAuthorityBanner } from "./CertificateAuthorityBanner";
import { LicenseBanner } from "./LicenseBanner";
import { UpdateBanner } from "./UpdateBanner";

/** The API caps a page at 500; past that the count reads "500+". */
const COUNT_LIMIT = 500;

/**
 * Open incidents in `scope`. The API filters by site only, so a customer
 * scope filters the sites client-side; the API has no count endpoint.
 */
async function openIncidents(scope: Scope, sites: Site[], signal: AbortSignal): Promise<string> {
  const page = await api.incidents(
    { state: "open", limit: COUNT_LIMIT, ...(scope.siteId && { site_id: scope.siteId }) },
    signal,
  );
  const inScope = scope.customerId && !scope.siteId
    ? page.incidents.filter((i) => sites.some((s) => s.id === i.site_id && s.customer_id === scope.customerId))
    : page.incidents;
  return page.next_cursor ? `${inScope.length}+` : String(inScope.length);
}

/**
 * One or two letters for `username`: the initials of its first two parts
 * (`lea.martin` gives "LM"), or its first letter.
 */
export function initials(username: string): string {
  const parts = username.split(/[\s._@-]+/).filter(Boolean);
  const letters = parts.length > 1 ? [parts[0]!, parts[1]!] : [parts[0] ?? "?"];
  return letters.map((part) => [...part][0]!.toUpperCase()).join("");
}

/**
 * The page chrome for a signed-in user: skip link, the 240 px sidebar
 * (`<nav aria-label="Main">`, a top bar with a menu under 800 px) and
 * `<main id="main">`. The sidebar follows the person's preferences
 * (ADR-0137): icons only or with labels (by default icons only under
 * 1100 px), items hidden or reordered. At its foot, the person's button
 * leads to Preferences (language, theme, sidebar, password, sign out),
 * next to a small collapse toggle. Uses the URL, so it needs a
 * `<Suspense>` boundary above it.
 */
export function AppShell({ me, children }: { me: Me; children: ReactNode }) {
  const pathname = usePathname();
  const search = useSearchParams().toString();
  const scope = useScope();
  const [customers, setCustomers] = useState<Customer[]>([]);
  const [sites, setSites] = useState<Site[]>([]);
  const [menuOpen, setMenuOpen] = useState(false);
  const [openedAt, setOpenedAt] = useState(pathname);
  const sidebar = usePreferences().preferences.sidebar;
  // The phone menu always has labels; above it, the person's choice or the width.
  const wide = useMediaQuery("(min-width: 800px)", true);
  const narrow = useMediaQuery("(max-width: 1099px)", false);
  const rail = wide && (sidebar?.collapsed ?? narrow);

  // A navigation closes the phone menu (state adjusted while rendering).
  if (openedAt !== pathname) {
    setOpenedAt(pathname);
    setMenuOpen(false);
  }

  // Where Back leads from a settings page: the last page of the app.
  const inSettings = insideSettings(pathname);
  const here = `${pathname}${search ? `?${search}` : ""}`;
  const [backHref, setBackHref] = useState<string | null>(null);
  if (!pathname.startsWith("/settings") && backHref !== here) setBackHref(here);
  const pressing = attention(useSettingsStatuses().statuses);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([api.customers(controller.signal), api.sites(controller.signal)]).then(
      ([c, s]) => {
        setCustomers(c.customers);
        setSites(s.sites);
      },
      (e: unknown) => {
        // Without the lists the switcher offers "All customers" only, or nothing.
        if (!isAbortError(e)) setCustomers([]);
      },
    );
    return () => controller.abort();
    // A second customer reloads `me`, and the lists with it (ADR-0104).
  }, [me.multi_customer]);

  // Another tab's theme change applies here too.
  useEffect(() => subscribeTheme(() => {}), []);

  const fetchCount = useCallback(
    (signal: AbortSignal) => openIncidents(scope, sites, signal),
    // The scope is compared by value; a new object each render must not refetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [scope.customerId, scope.siteId, sites],
  );
  const count = usePolledData(fetchCount).data;
  const countLabel = count && count !== "0" ? count : null;

  const roles = roleNames(me);
  // "500+" counts as many.
  const openCount = countLabel ? parseInt(countLabel, 10) : 0;

  return (
    <div className={styles.app}>
      <a href="#main" className={styles.skip}>
        {t("shell.skip")}
      </a>
      <div className={styles.topBar}>
        <Button
          className={styles.menuButton}
          aria-label={t("shell.menu")}
          aria-expanded={menuOpen}
          aria-controls="main-nav"
          onClick={() => setMenuOpen((open) => !open)}
        >
          <Icon name="menu" />
        </Button>
        <Link href={scopedHref("/", scope)} className={styles.topTitle}>
          MonitorFlow
        </Link>
        {countLabel && (
          <Link href={scopedHref("/incidents/?state=open", scope)} className={styles.topCount}>
            <Label tone="bad">{t("shell.open.short", { count: openCount, label: countLabel })}</Label>
          </Link>
        )}
      </div>
      <nav
        id="main-nav"
        className={cx(styles.side, menuOpen && styles.open, rail && styles.rail)}
        aria-label="Main"
        onKeyDown={(e) => {
          if (e.key === "Escape" && menuOpen) setMenuOpen(false);
        }}
      >
        <Link href={scopedHref("/", scope)} className={styles.brand} aria-label="MonitorFlow">
          <span className={styles.brandFull}>MonitorFlow</span>
          <span className={styles.brandShort} aria-hidden="true">
            MF
          </span>
        </Link>
        {inSettings ? (
          <SettingsSidebar me={me} pathname={pathname} backHref={backHref ?? scopedHref("/", scope)} />
        ) : (
          <>
            {(multiCustomer(me) || sites.length > 1) && (
              <ScopeSwitcher customers={customers} sites={sites} multiCustomer={multiCustomer(me)} />
            )}
            {arrangedNavigation(me, sidebar).map((section) => (
              <Fragment key={section.heading}>
                <div className={styles.heading}>{t(section.heading)}</div>
                {section.items.map((item) => (
                  <Link
                    key={item.label}
                    href={scopedHref(item.href!, scope)}
                    className={styles.item}
                    aria-current={isCurrent(item, pathname) ? "page" : undefined}
                  >
                    <Icon name={item.icon} />
                    <span className={styles.text}>{t(item.label)}</span>
                    {item.incidentCount && countLabel && (
                      <span className={styles.count} aria-label={t("shell.open.incidents", { count: openCount, label: countLabel })}>
                        {countLabel}
                      </span>
                    )}
                  </Link>
                ))}
              </Fragment>
            ))}
            <Link
              href={scopedHref(SETTINGS_ITEM.href!, scope)}
              className={cx(styles.item, styles.pinned)}
              aria-current={isCurrent(SETTINGS_ITEM, pathname) ? "page" : undefined}
            >
              <Icon name={SETTINGS_ITEM.icon} />
              <span className={styles.text}>{t(SETTINGS_ITEM.label)}</span>
              {pressing && (
                <span
                  className={cx(styles.count, pressing.tone === "warn" && styles.countWarn)}
                  aria-label={t("shell.settings.attention", { count: pressing.count })}
                >
                  {pressing.count}
                </span>
              )}
            </Link>
          </>
        )}
        <div className={styles.foot}>
          <Link
            href={PREFERENCES_PATH}
            className={styles.user}
            title={t("shell.account.title", { name: me.username, roles })}
            aria-current={pathname.startsWith(PREFERENCES_PATH) ? "page" : undefined}
          >
            <span className={styles.avatar} aria-hidden="true">
              {initials(me.username)}
            </span>
            <span className={cx(styles.text, styles.userName)}>{me.username}</span>
          </Link>
          <button
            type="button"
            className={styles.toggle}
            aria-expanded={!rail}
            aria-label={t(rail ? "shell.sidebar.expand" : "shell.sidebar.collapse")}
            title={t(rail ? "shell.sidebar.expand" : "shell.sidebar.collapse")}
            onClick={() => setPreference("sidebar", { ...sidebar, collapsed: !rail })}
          >
            <Icon name={rail ? "expand" : "collapse"} />
          </button>
        </div>
      </nav>
      <main id="main" className={styles.main} tabIndex={-1}>
        <StatusStrip />
        {administersEverything(me) && (
          <>
            <UpdateBanner />
            <CertificateAuthorityBanner />
            <LicenseBanner />
          </>
        )}
        {children}
      </main>
    </div>
  );
}
