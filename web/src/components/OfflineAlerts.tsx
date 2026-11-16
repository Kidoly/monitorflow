"use client";

import Link from "next/link";
import { useState } from "react";

import { rich, t } from "@/i18n";
import { api, describeError } from "@/lib/api";
import { formatSeconds } from "@/lib/format";
import { OFFLINE_AFTER_S } from "@/lib/incidents";
import { modifiesSite, useMe } from "@/lib/me";
import { SITES_PATH } from "@/lib/routes";
import { Button } from "@/components/ui/Button";
import { Label } from "@/components/ui/StatusLabel";

import styles from "./DetailPages.module.css";

type Kind = "sites" | "servers" | "relays";

interface Target {
  id: string;
  customer_id: string;
  /** The site's own ID for a site. */
  site_id: string;
  /** Absent is on. */
  offline_alerts?: boolean;
  site_offline_alerts?: boolean;
}

/**
 * Whether a site, server or relay opens an incident when it stops
 * reporting (ADR-0072), with Turn off and Turn on for operators of its
 * site. To tell nobody for a while, a silence keeps it watched (ADR-0079).
 */
export function OfflineAlerts({ kind, target, onChanged }: { kind: Kind; target: Target; onChanged: () => void }) {
  const me = useMe();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const on = target.offline_alerts !== false;
  if (target.site_offline_alerts === false) {
    return (
      <>
        <Label tone="neutral">{t("server.offline.off")}</Label>{" "}
        <span className={styles.muted}>
          {rich("server.offline.wholeSite", { link: <Link href={SITES_PATH}>{t("server.offline.sites")}</Link> })}
        </span>
      </>
    );
  }
  async function change() {
    setBusy(true);
    setError(null);
    try {
      await api.setOfflineAlerts(kind, target.id, !on);
      onChanged();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  const duration = formatSeconds(OFFLINE_AFTER_S);
  return (
    <>
      <Label tone={on ? "ok" : "neutral"}>{on ? t("server.offline.on") : t("server.offline.off")}</Label>{" "}
      <span className={styles.muted}>
        {on
          ? t(kind === "sites" ? "server.offline.whenSite" : "server.offline.when", { duration })
          : t("server.offline.none")}
      </span>{" "}
      {modifiesSite(me, target.customer_id, target.site_id) && (
        <Button size="small" disabled={busy} onClick={() => void change()}>
          {on ? t("server.offline.turnOff") : t("server.offline.turnOn")}
        </Button>
      )}
      {error && <span role="alert"> {error}</span>}
    </>
  );
}
