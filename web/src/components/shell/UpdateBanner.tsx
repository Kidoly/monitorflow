"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";

import { Banner } from "@/components/ui/PageState";
import { t } from "@/i18n";
import { api } from "@/lib/api";
import { SERVER_UPDATE_PATH } from "@/lib/routes";
import { inProgress } from "@/lib/serverUpdate";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Shell.module.css";

/** Releases come rarely: the daily check is what moves this. */
const REFRESH_MS = 60 * 60 * 1000;

/**
 * Tells global administrators, on every page, that a newer release exists
 * (ADR-0061). Nothing shows when there is none, or when the server cannot
 * tell: the update page has the rest.
 */
export function UpdateBanner() {
  const pathname = usePathname();
  const view = usePolledData(api.serverUpdate, { intervalMs: REFRESH_MS }).data;
  const onUpdatePage = pathname.startsWith(SERVER_UPDATE_PATH.slice(0, -1));
  if (!view?.available || onUpdatePage || inProgress(view.update, view.requested)) return null;
  return (
    <div className={styles.notice}>
      <Banner tone="info">
        <strong>{t("shell.update.available", { version: view.available.version })}</strong>{" "}
        {t("shell.update.running", { version: view.running_version })}{" "}
        <Link href={SERVER_UPDATE_PATH}>{t("shell.update.review")}</Link>
      </Banner>
    </div>
  );
}
