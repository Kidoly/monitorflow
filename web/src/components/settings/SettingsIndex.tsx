"use client";

import Link from "next/link";
import { useEffect, useId, useState } from "react";

import { rich, t } from "@/i18n";
import type { Me } from "@/lib/api";
import { useMe } from "@/lib/me";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { Page } from "@/components/ui/Page";
import { EmptyState, NoPermission, PageHeader } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";

import { Icon } from "../shell/icons";
import { type SettingsEntry, filterSettings, visibleSettings } from "./catalogue";
import styles from "./Settings.module.css";
import { type SettingsStatus, needsAttention } from "./status";
import { useSettingsStatuses } from "./SettingsStatus";

/** `/settings/`: every settings page `me` may open, in groups, with a search and where each stands. */
export function SettingsIndex() {
  const me = useMe();
  const { statuses, refresh } = useSettingsStatuses();
  const [query, setQuery] = useState("");
  const searchId = useId();

  // Coming back here after a change shows where it stands now.
  useEffect(() => refresh(), [refresh]);

  const groups = me ? visibleSettings(me) : [];
  if (!me || groups.length === 0) {
    return (
      <NoPermission title={t("settings.index.noPermission")}>
        {rich("settings.index.askAccess", { fleet: <Link href="/">{t("shell.nav.fleet")}</Link> })}
      </NoPermission>
    );
  }
  const shown = filterSettings(groups, query, me);

  return (
    <Page>
      <PageHeader title={t("shell.nav.settings")} meta={t("settings.index.meta")} />
      <div className={styles.search} role="search">
        <label htmlFor={searchId} className="visually-hidden">
          {t("shell.settings.search")}
        </label>
        <input
          id={searchId}
          type="search"
          className={cx(fieldClass, styles.searchField)}
          placeholder={t("shell.settings.searchPlaceholder")}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.preventDefault();
              setQuery("");
            }
          }}
          autoComplete="off"
          spellCheck={false}
        />
      </div>
      {shown.length === 0 ? (
        <EmptyState actions={<Button onClick={() => setQuery("")}>{t("settings.index.clearSearch")}</Button>}>
          {t("settings.index.noMatch", { query: query.trim() })}
        </EmptyState>
      ) : (
        shown.map((group) => (
          <section key={group.heading} className={styles.group} aria-labelledby={`${searchId}-${group.heading}`}>
            <h2 id={`${searchId}-${group.heading}`} className={styles.groupTitle}>
              {t(group.heading)}
            </h2>
            <ul className={styles.rows}>
              {group.entries.map((entry) => (
                <li key={entry.id}>
                  <SettingsRow entry={entry} me={me} status={statuses[entry.id]} />
                </li>
              ))}
            </ul>
          </section>
        ))
      )}
    </Page>
  );
}

function SettingsRow({ entry, me, status }: { entry: SettingsEntry; me: Me; status: SettingsStatus | undefined }) {
  return (
    <Link href={entry.href} className={styles.row}>
      <span className={styles.rowIcon}>
        <Icon name={entry.icon} />
      </span>
      <span className={styles.rowText}>
        <span className={styles.rowName}>{t(entry.label)}</span>
        {entry.description && <span className={styles.rowDescription}>{entry.description(me)}</span>}
        {status && <StatusLine status={status} />}
      </span>
      <span className={styles.chevron} aria-hidden="true">
        ›
      </span>
    </Link>
  );
}

/** Where a page stands; what needs someone says so in words, not only in color. */
export function StatusLine({ status }: { status: SettingsStatus }) {
  if (needsAttention(status)) {
    return (
      <span className={styles.rowStatus}>
        <Label tone={status.tone} small>
          {status.tone === "bad" ? t("common.state.critical") : t("common.state.warning")}
        </Label>{" "}
        <span className={styles[status.tone]}>{status.text}</span>
      </span>
    );
  }
  return <span className={cx(styles.rowStatus, status.tone === "ok" && styles.ok)}>{status.text}</span>;
}
