"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

import { Banner } from "@/components/ui/PageState";
import { api } from "@/lib/api";
import { licenseNotice } from "@/lib/license";
import { LICENSE_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Shell.module.css";

/** Expiry counts in days. */
const REFRESH_MS = 60 * 60 * 1000;

/**
 * Tells global administrators, on every page, when the Enterprise license
 * no longer covers adding central servers, once one was added (ADR-0100).
 * A single central server never sees it.
 */
export function LicenseBanner() {
  const pathname = usePathname();
  const view = usePolledData(api.license, { intervalMs: REFRESH_MS }).data;
  const notice = view ? licenseNotice(view) : null;
  if (!notice || pathname.startsWith(LICENSE_PATH.slice(0, -1))) return null;
  return (
    <div className={styles.notice}>
      <Banner tone={view?.state === "expires_soon" ? "warn" : "bad"}>
        <strong>{notice}</strong> <Link href={LICENSE_PATH}>See the license</Link>
      </Banner>
    </div>
  );
}
