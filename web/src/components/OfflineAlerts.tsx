"use client";

import Link from "next/link";
import { useState } from "react";

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
 * reporting (ADR-0057), with Mute and Turn on for operators of its site.
 */
export function OfflineAlerts({ kind, target, onChanged }: { kind: Kind; target: Target; onChanged: () => void }) {
  const me = useMe();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const on = target.offline_alerts !== false;
  if (target.site_offline_alerts === false) {
    return (
      <>
        <Label tone="neutral">Muted</Label>{" "}
        <span className={styles.muted}>
          for the whole site, in <Link href={SITES_PATH}>Sites</Link>
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
  const who = kind === "sites" ? "a server or relay here" : "it";
  return (
    <>
      <Label tone={on ? "ok" : "neutral"}>{on ? "On" : "Muted"}</Label>{" "}
      <span className={styles.muted}>
        {on ? `critical incident when ${who} sends nothing for ${formatSeconds(OFFLINE_AFTER_S)}` : "no incident when offline"}
      </span>{" "}
      {modifiesSite(me, target.customer_id, target.site_id) && (
        <Button size="small" disabled={busy} onClick={() => void change()}>
          {on ? "Mute" : "Turn on"}
        </Button>
      )}
      {error && <span role="alert"> {error}</span>}
    </>
  );
}
