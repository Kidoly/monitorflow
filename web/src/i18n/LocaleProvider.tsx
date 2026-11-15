"use client";

import { Fragment, type ReactNode, useEffect, useSyncExternalStore } from "react";

import { DEFAULT_LOCALE, type Locale, preferredLocale, setActiveLocale, subscribeLocale } from "./locale";

/** The language the page shows: English in the static HTML and while hydrating, then the person's. */
export function useLocale(): Locale {
  return useSyncExternalStore(subscribeLocale, preferredLocale, () => DEFAULT_LOCALE);
}

/**
 * Sets the language of `t` and the formatters, and `<html lang>`. A change
 * remounts the page under it: every text is drawn again in the new
 * language, at the cost of the page's state, as a switch is rare.
 */
export function LocaleProvider({ children }: { children: ReactNode }) {
  const locale = useLocale();
  // Set while rendering so the children below read it in this same render.
  setActiveLocale(locale);
  useEffect(() => {
    document.documentElement.lang = locale;
  }, [locale]);
  return <Fragment key={locale}>{children}</Fragment>;
}
