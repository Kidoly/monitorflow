"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { type ReactNode, useEffect, useState } from "react";

import { Banner, LoadingBlock } from "@/components/ui/PageState";
import { t } from "@/i18n";
import { ApiError, LOGIN_PATH, type Me, api, describeError, isAbortError, setUnauthorizedHandler } from "@/lib/api";
import { asset } from "@/lib/basePath";
import { MeContext } from "@/lib/me";
import { MOBILE_ON_CALL_PATH, MOBILE_PATH, MOBILE_PHONE_PATH, chooseFullView } from "@/lib/mobile";

import styles from "./Mobile.module.css";

/** Registers the service worker that shows notifications and acts on them (ADR-0186). */
export function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  if (typeof navigator === "undefined" || !("serviceWorker" in navigator)) return Promise.resolve(null);
  return navigator.serviceWorker.register(asset("/sw.js"), { scope: asset("/") }).catch(() => null);
}

/**
 * The phone pages: the signed-in person, a header, and three tabs at the
 * bottom within thumb's reach. A 401 sends the phone to the sign-in page,
 * which offers the sign-in provider when one is set (ADR-0145).
 */
export function MobileShell({ children }: { children: ReactNode }) {
  const router = useRouter();
  const pathname = usePathname();
  const [me, setMe] = useState<Me | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const restore = setUnauthorizedHandler(() => router.replace(LOGIN_PATH));
    const controller = new AbortController();
    api.me(controller.signal).then(setMe, (e: unknown) => {
      if (!isAbortError(e) && !(e instanceof ApiError && e.status === 401)) setError(describeError(e));
    });
    void registerServiceWorker();
    return () => {
      controller.abort();
      restore();
    };
  }, [router]);

  const tabs = [
    { href: MOBILE_PATH, label: t("mobile.nav.alerts") },
    { href: MOBILE_ON_CALL_PATH, label: t("mobile.nav.onCall") },
    { href: MOBILE_PHONE_PATH, label: t("mobile.nav.phone") },
  ];
  const current = (href: string) =>
    href === MOBILE_PATH ? pathname === MOBILE_PATH || pathname.startsWith("/mobile/incident") : pathname.startsWith(href);

  return (
    <div className={styles.shell}>
      <header className={styles.header}>
        <p className={styles.headerTitle}>MonitorFlow</p>
        <Link href="/" className={styles.navItem} onClick={chooseFullView}>
          {t("mobile.fullView")}
        </Link>
      </header>
      <main id="main" className={styles.main}>
        {error ? (
          <Banner tone="bad">{error}</Banner>
        ) : me ? (
          <MeContext.Provider value={me}>{children}</MeContext.Provider>
        ) : (
          <LoadingBlock label={t("common.loading")} />
        )}
      </main>
      <nav className={styles.nav} aria-label={t("mobile.nav.label")}>
        {tabs.map((tab) => (
          <Link key={tab.href} href={tab.href} className={styles.navItem} aria-current={current(tab.href) ? "page" : undefined}>
            {tab.label}
          </Link>
        ))}
      </nav>
    </div>
  );
}
