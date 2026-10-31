"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { Fragment, type ReactNode, useCallback, useEffect, useState } from "react";

import { Button } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { Label } from "@/components/ui/StatusLabel";
import { type Customer, LOGIN_PATH, type Me, type Site, api, describeError, isAbortError } from "@/lib/api";
import { administersEverything } from "@/lib/me";
import { type Scope, scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";

import { Icon } from "./icons";
import { isCurrent, visibleNavigation } from "./navigation";
import styles from "./Shell.module.css";
import { ScopeSwitcher } from "./ScopeSwitcher";
import { ThemeToggle } from "./ThemeToggle";
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
 * The page chrome for a signed-in user: skip link, the 240 px sidebar
 * (`<nav aria-label="Main">`, icons only under 1100 px, a top bar with a
 * menu under 800 px) and `<main id="main">`. Uses the URL, so it needs a
 * `<Suspense>` boundary above it.
 */
export function AppShell({ me, children }: { me: Me; children: ReactNode }) {
  const pathname = usePathname();
  const router = useRouter();
  const scope = useScope();
  const [customers, setCustomers] = useState<Customer[]>([]);
  const [sites, setSites] = useState<Site[]>([]);
  const [menuOpen, setMenuOpen] = useState(false);
  const [openedAt, setOpenedAt] = useState(pathname);
  const [error, setError] = useState<string | null>(null);

  // A navigation closes the phone menu (state adjusted while rendering).
  if (openedAt !== pathname) {
    setOpenedAt(pathname);
    setMenuOpen(false);
  }

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([api.customers(controller.signal), api.sites(controller.signal)]).then(
      ([c, s]) => {
        setCustomers(c.customers);
        setSites(s.sites);
      },
      (e: unknown) => {
        // Without the lists the switcher offers "All customers" only.
        if (!isAbortError(e)) setCustomers([]);
      },
    );
    return () => controller.abort();
  }, []);

  const fetchCount = useCallback(
    (signal: AbortSignal) => openIncidents(scope, sites, signal),
    // The scope is compared by value; a new object each render must not refetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [scope.customerId, scope.siteId, sites],
  );
  const count = usePolledData(fetchCount).data;
  const countLabel = count && count !== "0" ? count : null;

  async function signOut() {
    setError(null);
    try {
      await api.logout();
      router.replace(LOGIN_PATH);
    } catch (e) {
      setError(`Sign-out failed. ${describeError(e)}`);
    }
  }

  const roles = [...new Set(me.roles.map((grant) => grant.role))].join(", ") || "no role";

  return (
    <div className={styles.app}>
      <a href="#main" className={styles.skip}>
        Skip to content
      </a>
      <div className={styles.topBar}>
        <Button
          className={styles.menuButton}
          aria-label="Menu"
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
            <Label tone="bad">{countLabel} open</Label>
          </Link>
        )}
      </div>
      <nav
        id="main-nav"
        className={cx(styles.side, menuOpen && styles.open)}
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
        <ScopeSwitcher customers={customers} sites={sites} />
        {visibleNavigation(me).map((section) => (
          <Fragment key={section.heading}>
            <div className={styles.heading}>{section.heading}</div>
            {section.items.map((item) => (
              <Link
                key={item.label}
                href={scopedHref(item.href!, scope)}
                className={styles.item}
                aria-current={isCurrent(item, pathname) ? "page" : undefined}
              >
                <Icon name={item.icon} className={cx(item.iconWhenCollapsed && styles.wideHidden)} />
                <span className={styles.text}>{item.label}</span>
                {item.incidentCount && countLabel && (
                  <span className={styles.count} aria-label={`${countLabel} open incidents`}>
                    {countLabel}
                  </span>
                )}
              </Link>
            ))}
          </Fragment>
        ))}
        <div className={styles.foot}>
          <span className={styles.who}>
            {me.username} · {roles}
          </span>
          <button type="button" className={styles.signOut} onClick={() => void signOut()}>
            <Icon name="signOut" />
            <span className={styles.signOutText}>Sign out</span>
          </button>
        </div>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <ThemeToggle />
      </nav>
      <main id="main" className={styles.main} tabIndex={-1}>
        {administersEverything(me) && <UpdateBanner />}
        {children}
      </main>
    </div>
  );
}
