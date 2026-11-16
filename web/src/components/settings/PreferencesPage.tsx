"use client";

import { useId } from "react";

import { fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { Page } from "@/components/ui/Page";
import { Banner, PageHeader } from "@/components/ui/PageState";
import { t } from "@/i18n";
import { LOCALES, LOCALE_NAMES, type Locale } from "@/i18n/locale";
import { useLocale } from "@/i18n/LocaleProvider";
import { useMe } from "@/lib/me";
import { setLanguage, setTheme, usePreferences } from "@/lib/preferences";
import { THEMES, type Theme } from "@/lib/theme";

import { SidebarEditor } from "../shell/SidebarEditor";
import styles from "./Settings.module.css";

/**
 * `/settings/preferences/`: the signed-in person's own choices (ADR-0137),
 * for everyone, distinct from the server-wide settings around it. Each
 * change applies at once and is saved with the account.
 */
export function PreferencesPage() {
  const me = useMe();
  const { preferences, save } = usePreferences();
  const locale = useLocale();
  const languageId = useId();
  const theme: Theme = preferences.theme ?? "system";
  if (!me) return null;

  return (
    <Page>
      <PageHeader
        title={t("settings.preferences.label")}
        meta={save === "saving" ? t("settings.preferences.saving") : t("settings.preferences.meta")}
      />
      {save === "error" && <Banner tone="bad">{t("settings.preferences.saveFailed")}</Banner>}

      <section className={styles.group} aria-labelledby="preferences-appearance">
        <h2 id="preferences-appearance" className={styles.groupTitle}>
          {t("settings.preferences.appearance")}
        </h2>
        <fieldset className={styles.choices}>
          <legend className={styles.choiceLegend}>{t("settings.preferences.theme")}</legend>
          {THEMES.map((option) => (
            <label key={option} className={styles.choice}>
              <input
                type="radio"
                name="theme"
                value={option}
                checked={theme === option}
                onChange={() => setTheme(option)}
              />
              {t(`shell.theme.${option}`)}
            </label>
          ))}
        </fieldset>
        <div className={styles.choices}>
          <label htmlFor={languageId} className={styles.choiceLegend}>
            {t("settings.preferences.language")}
          </label>
          <select
            id={languageId}
            className={cx(fieldClass, styles.choiceSelect)}
            value={locale}
            onChange={(e) => setLanguage(e.target.value as Locale)}
          >
            {LOCALES.map((option) => (
              <option key={option} value={option} lang={option}>
                {LOCALE_NAMES[option]}
              </option>
            ))}
          </select>
        </div>
      </section>

      <section className={styles.group} aria-labelledby="sidebar">
        <h2 id="sidebar" className={styles.groupTitle}>
          {t("settings.preferences.sidebar")}
        </h2>
        <SidebarEditor me={me} />
      </section>
    </Page>
  );
}
