"use client";

import Link from "next/link";
import { useCallback } from "react";

import { EmptyState, ErrorState } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatSince } from "@/components/ui/time";
import { t } from "@/i18n";
import { api, describeError } from "@/lib/api";
import { useMe } from "@/lib/me";
import { onCallNow } from "@/lib/mobile";
import { ON_CALL_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./Mobile.module.css";

/** Who is on call now in each schedule the person sees, theirs first. */
export function MobileOnCall() {
  const me = useMe();
  const fetcher = useCallback((signal: AbortSignal) => api.onCallSchedules(signal), []);
  const { data, error } = usePolledData(fetcher);
  const now = new Date();
  const rows = data && me ? onCallNow(data.schedules, me.id) : [];
  const names = (people: string[]) => people.join(", ");

  return (
    <>
      <h1 className={styles.title}>{t("mobile.onCallTitle")}</h1>
      {error !== null && !data && <ErrorState what={t("mobile.onCallWhat")} error={describeError(error)} />}
      {data && rows.length === 0 && <EmptyState title={t("mobile.noSchedules")}>{t("mobile.noSchedulesHint")}</EmptyState>}
      <ul className={styles.list}>
        {rows.map((row) => (
          <li key={row.id} className={styles.card}>
            <span className={styles.cardTop}>
              <span className={styles.cardTitle}>{row.name}</span>
              {row.me && (
                <Label tone="info" small>
                  {t("mobile.you")}
                </Label>
              )}
              {row.gap && (
                <Label tone="warn" small>
                  {t("mobile.gap")}
                </Label>
              )}
            </span>
            <span>
              {row.people.length > 0 ? t("mobile.onCallNow", { names: names(row.people) }) : t("mobile.nobodyOnCall")}
            </span>
            {row.until && (
              <span className={styles.muted}>
                {row.next.length > 0
                  ? t("mobile.nextAt", { names: names(row.next), time: formatSince(new Date(row.until), now) })
                  : t("mobile.until", { time: formatSince(new Date(row.until), now) })}
              </span>
            )}
          </li>
        ))}
      </ul>
      <Link href={ON_CALL_PATH} className={styles.link}>
        {t("mobile.manageOnCall")}
      </Link>
    </>
  );
}
