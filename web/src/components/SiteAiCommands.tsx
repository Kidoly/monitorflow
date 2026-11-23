"use client";

import { useState } from "react";

import { t } from "@/i18n";
import { type Site, type SiteSourcesChange, api, describeError } from "@/lib/api";
import { administersSite, useMe } from "@/lib/me";

import admin from "./AdminPages.module.css";
import styles from "./PackageSources.module.css";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner } from "./ui/PageState";

/**
 * AI commands on or off for every server of a site at once, for its administrators (ADR-0098).
 * Servers already as asked are left alone; those that cannot be switched are listed with why.
 */
export function SiteAiCommands({ site }: { site: Site }) {
  const me = useMe();
  const [result, setResult] = useState<{ on: boolean; change: SiteSourcesChange } | null>(null);
  if (!administersSite(me, site.customer_id, site.id) || site.server_count === 0) return null;
  const action = (on: boolean) => {
    return (
      <InlineConfirm
        trigger={on ? t("fleet.siteAi.onTrigger") : t("fleet.siteAi.offTrigger")}
        triggerVariant={on ? "default" : "danger"}
        title={on ? t("fleet.siteAi.onTitle", { site: site.name }) : t("fleet.siteAi.offTitle", { site: site.name })}
        consequence={on ? t("fleet.siteAi.onConsequence") : t("fleet.siteAi.offConsequence")}
        confirmLabel={on ? t("fleet.siteAi.onConfirm") : t("fleet.siteAi.offConfirm")}
        confirmVariant={on ? "k" : "danger"}
        pendingLabel={t("fleet.siteAi.sending")}
        onConfirm={async () => setResult({ on, change: await api.switchSiteAiCommands(site.id, on) })}
        describeError={(e) => t("fleet.siteAi.failed", { error: describeError(e) })}
      />
    );
  };
  return (
    <section className={admin.section} aria-labelledby="site-ai-commands-title">
      <div className={admin.sectionHead}>
        <h2 id="site-ai-commands-title" className={admin.sectionTitle}>
          {t("fleet.siteAi.title")}
        </h2>
      </div>
      <div className={styles.body}>
        <p className={styles.note}>
          {t("fleet.siteAi.note", { site: site.name })}
        </p>
        {result && <SwitchResult {...result} />}
        <div className={styles.actions}>
          {action(true)}
          {action(false)}
        </div>
      </div>
    </section>
  );
}

/** What a switch of many servers did: how many change, and those that cannot, with why. */
export function SwitchResult({ on, change }: { on: boolean; change: SiteSourcesChange }) {
  const count = change.queued.length;
  return (
    <Banner tone={change.queued.length > 0 ? "ok" : "warn"} role="status">
      {change.queued.length > 0
        ? t(on ? "fleet.siteAi.turningOn" : "fleet.siteAi.turningOff", { count })
        : t(on ? "fleet.siteAi.noneOn" : "fleet.siteAi.noneOff")}
      {change.skipped.length > 0 && (
        <ul className={styles.skipped}>
          {change.skipped.map((s) => (
            <li key={s.id}>
              {t("fleet.siteAi.skipped", { hostname: s.hostname, reason: s.reason })}
            </li>
          ))}
        </ul>
      )}
    </Banner>
  );
}
