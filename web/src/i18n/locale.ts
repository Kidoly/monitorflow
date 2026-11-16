/** The languages of the web interface. English is the reference catalog. */
export const LOCALES = ["en", "fr"] as const;

export type Locale = (typeof LOCALES)[number];

/** What a missing choice, an unknown browser language and the static HTML use. */
export const DEFAULT_LOCALE: Locale = "en";

/** Each language in its own words, so anyone finds theirs in the switch. */
export const LOCALE_NAMES: Record<Locale, string> = { en: "English", fr: "Français" };

/** localStorage key of the picked language; absent means the browser's. */
export const LOCALE_KEY = "monitorflow.locale";

const isLocale = (value: string | null | undefined): value is Locale =>
  (LOCALES as readonly (string | null | undefined)[]).includes(value);

/** The first of `tags` (BCP 47, like `fr-CA`) the interface speaks, by its language. */
export function matchLocale(tags: readonly string[]): Locale | null {
  for (const tag of tags) {
    const language = tag.split("-")[0]?.toLowerCase();
    if (isLocale(language)) return language;
  }
  return null;
}

/** The stored choice, or null when unset, unreadable or unknown. */
export function storedLocale(): Locale | null {
  try {
    const value = localStorage.getItem(LOCALE_KEY);
    return isLocale(value) ? value : null;
  } catch {
    return null;
  }
}

/** The browser's languages in its order of preference. */
function browserLanguages(): readonly string[] {
  if (typeof navigator === "undefined") return [];
  return navigator.languages?.length ? navigator.languages : [navigator.language];
}

// The choice when storage refuses it, for this page only.
let unstored: Locale | null = null;

/** The person's choice, else the browser's language, else English. */
export function preferredLocale(): Locale {
  return storedLocale() ?? unstored ?? matchLocale(browserLanguages()) ?? DEFAULT_LOCALE;
}

const listeners = new Set<() => void>();

/** For `useSyncExternalStore`: changes by `applyLocale` here or in another tab, or of the browser's languages. */
export function subscribeLocale(listener: () => void): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key === LOCALE_KEY || event.key === null) listener();
  };
  listeners.add(listener);
  window.addEventListener("storage", onStorage);
  window.addEventListener("languagechange", listener);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("storage", onStorage);
    window.removeEventListener("languagechange", listener);
  };
}

/** Stores `locale` as the person's choice, for this browser. */
export function applyLocale(locale: Locale) {
  try {
    localStorage.setItem(LOCALE_KEY, locale);
  } catch {
    // Storage may be disabled; the language still applies to this page.
    unstored = locale;
  }
  listeners.forEach((listener) => listener());
}

let active: Locale = DEFAULT_LOCALE;

/**
 * The language `t` and the formatters use. `LocaleProvider` sets it while
 * rendering and remounts the page when it changes; tests set it directly.
 */
export function activeLocale(): Locale {
  return active;
}

export function setActiveLocale(locale: Locale) {
  active = locale;
}

/** The locale dates are written in: English ones read day first, "30 November 2027", as the design has them. */
export function dateLocale(): string {
  return active === "en" ? "en-GB" : active;
}
