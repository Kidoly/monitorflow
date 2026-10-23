/** A theme the user picked; "system" follows `prefers-color-scheme`. */
export type Theme = "light" | "dark" | "system";

/** localStorage key of the picked theme; absent means "system". */
export const THEME_KEY = "monitorflow.theme";

/** The order the theme button cycles through. */
export const THEMES: readonly Theme[] = ["system", "light", "dark"];

export const THEME_LABELS: Record<Theme, string> = { system: "System", light: "Light", dark: "Dark" };

/**
 * Runs in `<head>` before the first paint so a stored theme never flashes
 * the default one. Kept in sync with `applyTheme` by hand: it is a string.
 */
export const THEME_SCRIPT = `(function(){try{var t=localStorage.getItem(${JSON.stringify(THEME_KEY)});if(t==="light"||t==="dark")document.documentElement.setAttribute("data-theme",t)}catch(e){}})()`;

/** The stored theme; "system" when unset, unreadable or unknown. */
export function storedTheme(): Theme {
  try {
    const value = localStorage.getItem(THEME_KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

const listeners = new Set<() => void>();

/** For `useSyncExternalStore`: changes by `applyTheme` here or in another tab. */
export function subscribeTheme(listener: () => void): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key !== THEME_KEY && event.key !== null) return;
    const theme = storedTheme();
    if (theme === "system") document.documentElement.removeAttribute("data-theme");
    else document.documentElement.setAttribute("data-theme", theme);
    listener();
  };
  listeners.add(listener);
  window.addEventListener("storage", onStorage);
  return () => {
    listeners.delete(listener);
    window.removeEventListener("storage", onStorage);
  };
}

/** Sets `data-theme` on `<html>` (removed for "system") and stores the choice. */
export function applyTheme(theme: Theme) {
  const root = document.documentElement;
  try {
    if (theme === "system") {
      localStorage.removeItem(THEME_KEY);
    } else {
      localStorage.setItem(THEME_KEY, theme);
    }
  } catch {
    // Storage may be disabled; the theme still applies to this page.
  }
  if (theme === "system") {
    root.removeAttribute("data-theme");
  } else {
    root.setAttribute("data-theme", theme);
  }
  listeners.forEach((listener) => listener());
}

/** The theme after `theme` in `THEMES`. */
export function nextTheme(theme: Theme): Theme {
  return THEMES[(THEMES.indexOf(theme) + 1) % THEMES.length]!;
}
