"use client";

import { cx } from "@/components/ui/cx";
import { fieldClass } from "@/components/ui/Button";
import { t } from "@/i18n";
import { LOCALES, LOCALE_NAMES, type Locale } from "@/i18n/locale";
import { useLocale } from "@/i18n/LocaleProvider";
import { setLanguage } from "@/lib/preferences";

import { Icon } from "./icons";
import styles from "./Shell.module.css";

/**
 * The language of the interface, kept with the person's preferences. Each language is named
 * in its own words; the icon alone stands for it in the narrow sidebar.
 */
export function LanguageSelect() {
  const locale = useLocale();
  return (
    <label className={styles.language} title={t("shell.language")}>
      <Icon name="language" />
      <span className="visually-hidden">{t("shell.language")}</span>
      <select
        className={cx(fieldClass, styles.languageSelect)}
        value={locale}
        onChange={(e) => setLanguage(e.target.value as Locale)}
      >
        {LOCALES.map((option) => (
          <option key={option} value={option} lang={option}>
            {LOCALE_NAMES[option]}
          </option>
        ))}
      </select>
    </label>
  );
}
