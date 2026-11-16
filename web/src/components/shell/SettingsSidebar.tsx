"use client";

import Link from "next/link";
import { Fragment, useId, useState } from "react";

import { t } from "@/i18n";
import type { Me } from "@/lib/api";
import { SETTINGS_PATH } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { cx } from "@/components/ui/cx";
import { fieldClass } from "@/components/ui/Button";
import { Label } from "@/components/ui/StatusLabel";
import { currentSettingsEntry, filterSettings, visibleSettings } from "@/components/settings/catalogue";
import { needsAttention } from "@/components/settings/status";
import { useSettingsStatuses } from "@/components/settings/SettingsStatus";

import { Icon } from "./icons";
import styles from "./Shell.module.css";

/**
 * The sidebar inside a settings page: Back to where one was in the app,
 * then the settings list, grouped and filtered as on the Settings page,
 * the current page marked. What needs someone says so in a word.
 */
export function SettingsSidebar({ me, pathname, backHref }: { me: Me; pathname: string; backHref: string }) {
  const { statuses } = useSettingsStatuses();
  // Kept in every link, as in the app sidebar, so Back and the app find it again.
  const scope = useScope();
  const [query, setQuery] = useState("");
  const searchId = useId();
  const current = currentSettingsEntry(pathname);
  const groups = filterSettings(visibleSettings(me), query, me);

  return (
    <>
      <Link href={backHref} className={cx(styles.item, styles.back)}>
        <Icon name="back" />
        <span className={styles.text}>{t("shell.settings.back")}</span>
      </Link>
      <Link href={scopedHref(SETTINGS_PATH, scope)} className={styles.settingsTitle}>
        {t("shell.nav.settings")}
      </Link>
      <label htmlFor={searchId} className="visually-hidden">
        {t("shell.settings.search")}
      </label>
      <input
        id={searchId}
        type="search"
        className={cx(fieldClass, styles.settingsSearch)}
        placeholder={t("shell.settings.searchPlaceholder")}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Escape" && query) {
            // The phone menu closes on Escape; a filled search clears first.
            e.stopPropagation();
            setQuery("");
          }
        }}
        autoComplete="off"
        spellCheck={false}
      />
      {groups.length === 0 && <p className={styles.noMatch}>{t("shell.settings.noMatch")}</p>}
      {groups.map((group) => (
        <Fragment key={group.heading}>
          <div className={styles.heading}>{t(group.heading)}</div>
          {group.entries.map((entry) => {
            const status = statuses[entry.id];
            return (
              <Link
                key={entry.id}
                href={scopedHref(entry.href, scope)}
                className={styles.item}
                aria-current={entry === current ? "page" : undefined}
              >
                <Icon name={entry.icon} />
                <span className={cx(styles.text, styles.ellipsis)}>{t(entry.label)}</span>
                {needsAttention(status) && (
                  <span className={styles.flag}>
                    <Label tone={status!.tone} small>
                      {status!.tone === "bad" ? t("common.state.critical") : t("common.state.warning")}
                    </Label>
                  </span>
                )}
              </Link>
            );
          })}
        </Fragment>
      ))}
    </>
  );
}
