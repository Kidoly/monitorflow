"use client";

import { useState } from "react";

import { type Site, type SiteSourcesChange, api, describeError } from "@/lib/api";
import { administersSite, useMe } from "@/lib/me";

import admin from "./AdminPages.module.css";
import styles from "./PackageSources.module.css";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner } from "./ui/PageState";

const count = (n: number) => `${n} ${n === 1 ? "server" : "servers"}`;

/**
 * AI commands on or off for every server of a site at once, for its administrators (ADR-0098).
 * Servers already as asked are left alone; those that cannot be switched are listed with why.
 */
export function SiteAiCommands({ site }: { site: Site }) {
  const me = useMe();
  const [result, setResult] = useState<{ on: boolean; change: SiteSourcesChange } | null>(null);
  if (!administersSite(me, site.customer_id, site.id) || site.server_count === 0) return null;
  const action = (on: boolean) => {
    const word = on ? "on" : "off";
    return (
      <InlineConfirm
        trigger={`Turn ${word} for all…`}
        triggerVariant={on ? "default" : "danger"}
        title={`Turn AI commands ${word} on every server of ${site.name}?`}
        consequence={
          on
            ? "Only servers installed with “Allow AI commands to be turned on from the web interface” turn on; the others are listed. The channels covering them are told."
            : "AI's commands are refused on them and their incidents go to a person. The channels covering them are told."
        }
        confirmLabel={`Turn ${word} for all`}
        confirmVariant={on ? "k" : "danger"}
        pendingLabel="Sending…"
        onConfirm={async () => setResult({ on, change: await api.switchSiteAiCommands(site.id, on) })}
        describeError={(e) => `Nothing changed. ${describeError(e)}`}
      />
    );
  };
  return (
    <section className={admin.section} aria-labelledby="site-ai-commands-title">
      <div className={admin.sectionHead}>
        <h2 id="site-ai-commands-title" className={admin.sectionTitle}>
          AI commands
        </h2>
      </div>
      <div className={styles.body}>
        <p className={styles.note}>
          Turn AI commands on or off for every server of {site.name}. Each server&apos;s page shows where it stands.
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

function SwitchResult({ on, change }: { on: boolean; change: SiteSourcesChange }) {
  const word = on ? "on" : "off";
  return (
    <Banner tone={change.queued.length > 0 ? "ok" : "warn"} role="status">
      {change.queued.length > 0
        ? `Turning ${word} on ${count(change.queued.length)}. Each one changes at its next contact.`
        : `No server needed turning ${word}, or none could be.`}
      {change.skipped.length > 0 && (
        <ul className={styles.skipped}>
          {change.skipped.map((s) => (
            <li key={s.id}>
              {s.hostname}: {s.reason}
            </li>
          ))}
        </ul>
      )}
    </Banner>
  );
}
