"use client";

import { useSyncExternalStore } from "react";

import { Button } from "@/components/ui/Button";
import { t } from "@/i18n";
import { setTheme } from "@/lib/preferences";
import { type Theme, nextTheme, storedTheme, subscribeTheme } from "@/lib/theme";

import { Icon } from "./icons";
import styles from "./Shell.module.css";

/**
 * Cycles System → Light → Dark. The stored choice is applied before the
 * first paint by `THEME_SCRIPT`; this button reads it and changes it
 * through the person's preferences (ADR-0137).
 */
export function ThemeToggle() {
  // "system" while hydrating matches the static HTML.
  const theme = useSyncExternalStore(subscribeTheme, storedTheme, (): Theme => "system");

  return (
    <Button className={styles.themeButton} onClick={() => setTheme(nextTheme(theme))} title={t("shell.theme.switch")}>
      <Icon name="theme" />
      <span className={styles.themeText}>{t("shell.theme.current", { name: t(`shell.theme.${theme}`) })}</span>
    </Button>
  );
}
