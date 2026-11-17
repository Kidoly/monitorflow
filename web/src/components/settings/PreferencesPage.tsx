"use client";

import { useRouter } from "next/navigation";
import { useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { Page } from "@/components/ui/Page";
import { Banner, PageHeader } from "@/components/ui/PageState";
import { t } from "@/i18n";
import { LOCALES, LOCALE_NAMES, type Locale } from "@/i18n/locale";
import { useLocale } from "@/i18n/LocaleProvider";
import { LOGIN_PATH, api, describeError } from "@/lib/api";
import { roleNames, useMe } from "@/lib/me";
import { setLanguage, setTheme, usePreferences } from "@/lib/preferences";
import { THEMES, type Theme } from "@/lib/theme";

import { SidebarEditor } from "../shell/SidebarEditor";
import { PasswordForm } from "./PasswordForm";
import styles from "./Settings.module.css";

/**
 * `/settings/preferences/`: the signed-in person's own choices (ADR-0137),
 * for everyone, distinct from the server-wide settings around it. The
 * sidebar's user button leads here, so it also holds the password
 * (ADR-0141) and Sign out. Each preference applies at once and is saved
 * with the account.
 */
export function PreferencesPage() {
  const me = useMe();
  const router = useRouter();
  const { preferences, save } = usePreferences();
  const locale = useLocale();
  const languageId = useId();
  const [signOutError, setSignOutError] = useState<string | null>(null);
  const theme: Theme = preferences.theme ?? "system";
  if (!me) return null;

  async function signOut() {
    setSignOutError(null);
    try {
      await api.logout();
      router.replace(LOGIN_PATH);
    } catch (e) {
      setSignOutError(t("shell.signOutFailed", { error: describeError(e) }));
    }
  }

  const roles = roleNames(me);

  return (
    <Page>
      <PageHeader
        title={t("settings.preferences.label")}
        meta={save === "saving" ? t("settings.preferences.saving") : undefined}
        sub={t("settings.preferences.account", { name: me.username, roles })}
        actions={<Button onClick={() => void signOut()}>{t("shell.signOut")}</Button>}
      />
      {signOutError && <Banner tone="bad">{signOutError}</Banner>}
      {save === "error" && <Banner tone="bad">{t("settings.preferences.saveFailed")}</Banner>}

      <section className={styles.group} aria-labelledby="preferences-language">
        <h2 id="preferences-language" className={styles.groupTitle}>
          <label htmlFor={languageId}>{t("settings.preferences.language")}</label>
        </h2>
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
      </section>

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
      </section>

      <section className={styles.group} aria-labelledby="sidebar">
        <h2 id="sidebar" className={styles.groupTitle}>
          {t("settings.preferences.sidebar")}
        </h2>
        <SidebarEditor me={me} />
      </section>

      <section className={styles.group} aria-labelledby="preferences-password">
        <h2 id="preferences-password" className={styles.groupTitle}>
          {t("settings.preferences.password")}
        </h2>
        <PasswordForm username={me.username} />
      </section>
    </Page>
  );
}
