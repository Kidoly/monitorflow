"use client";

import { useSyncExternalStore } from "react";

import { Button } from "@/components/ui/Button";
import { THEME_LABELS, type Theme, applyTheme, nextTheme, storedTheme, subscribeTheme } from "@/lib/theme";

import { Icon } from "./icons";
import styles from "./Shell.module.css";

/**
 * Cycles System → Light → Dark. The stored choice is applied before the
 * first paint by `THEME_SCRIPT`; this button only reads and changes it.
 */
export function ThemeToggle() {
  // "system" while hydrating matches the static HTML.
  const theme = useSyncExternalStore(subscribeTheme, storedTheme, (): Theme => "system");

  return (
    <Button className={styles.themeButton} onClick={() => applyTheme(nextTheme(theme))} title="Switch theme">
      <Icon name="theme" />
      <span className={styles.themeText}>Theme: {THEME_LABELS[theme]}</span>
    </Button>
  );
}
