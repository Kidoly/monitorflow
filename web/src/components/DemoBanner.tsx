"use client";

import { usePathname, useRouter } from "next/navigation";
import { useEffect } from "react";

import { t } from "@/i18n";

import styles from "./DemoBanner.module.css";

/** Where the banner's links lead; the website repository sets the first at build time. */
export const DEMO_SITE_URL = process.env.NEXT_PUBLIC_MONITORFLOW_SITE_URL || "https://monitorflow.eu";
export const DEMO_SOURCE_URL = "https://github.com/Kidoly/monitorflow";

/** Pages of signing in, which the demo skips: its user is always signed in. */
const SIGN_IN = ["/login/", "/setup/", "/welcome/"];

/**
 * The static demo's banner (docs/demo.md): always on top, saying the data
 * is made up, with links to the website and the source code. It also sends
 * the sign-in pages to the fleet.
 */
export function DemoBanner() {
  const pathname = usePathname();
  const router = useRouter();
  useEffect(() => {
    if (SIGN_IN.includes(pathname) || SIGN_IN.includes(`${pathname}/`)) router.replace("/");
  }, [pathname, router]);

  return (
    <div className={styles.banner} role="note">
      <span className={styles.text}>{t("common.demo.banner")}</span>
      <a href={DEMO_SITE_URL}>{t("common.demo.website")}</a>
      <a href={DEMO_SOURCE_URL} rel="noopener">
        {t("common.demo.source")}
      </a>
    </div>
  );
}
