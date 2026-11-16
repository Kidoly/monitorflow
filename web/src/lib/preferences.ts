"use client";

import { useSyncExternalStore } from "react";

import { type Locale, applyLocale, storedLocale } from "@/i18n/locale";
import { type Preferences, type SidebarPreferences, api, isAbortError } from "@/lib/api";
import { applyTheme, storedTheme } from "@/lib/theme";

export type { Preferences, SidebarPreferences } from "@/lib/api";

/**
 * The signed-in person's preferences (ADR-0137), kept by the server so
 * they follow the person, and cached in this browser for the first paint.
 *
 * `startPreferences` loads them once after sign-in; a browser choice the
 * server does not have yet (theme, language) is written to it once. A
 * change applies at once and is sent after `SAVE_DELAY` ms of quiet.
 */

/** How long a change waits for the next one before it is sent. */
export const SAVE_DELAY = 400;

/** localStorage key of the cached sidebar preferences of `user`. */
export const sidebarKey = (user: string) => `monitorflow.sidebar.${user}`;

export type SaveState = "idle" | "saving" | "error";

interface State {
  preferences: Preferences;
  /** Whether the server's document has been read. */
  loaded: boolean;
  save: SaveState;
}

const EMPTY: State = { preferences: {}, loaded: false, save: "idle" };

let state: State = EMPTY;
let user: string | null = null;
let controller: AbortController | null = null;
let timer: ReturnType<typeof setTimeout> | null = null;
/** Keys changed before the server's document arrived: they win over it. */
let changedEarly = new Set<keyof Preferences>();

const listeners = new Set<() => void>();

function set(next: Partial<State>) {
  state = { ...state, ...next };
  listeners.forEach((listener) => listener());
}

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The cached sidebar preferences of `user`, or undefined. */
function cachedSidebar(id: string): SidebarPreferences | undefined {
  try {
    const raw = localStorage.getItem(sidebarKey(id));
    if (!raw) return undefined;
    const value: unknown = JSON.parse(raw);
    return value && typeof value === "object" && !Array.isArray(value) ? (value as SidebarPreferences) : undefined;
  } catch {
    return undefined;
  }
}

function cacheSidebar(id: string, sidebar: SidebarPreferences | undefined) {
  try {
    if (sidebar) localStorage.setItem(sidebarKey(id), JSON.stringify(sidebar));
    else localStorage.removeItem(sidebarKey(id));
  } catch {
    // Storage may be disabled; the server still has them.
  }
}

/** What this browser holds before the server answers: its theme, language and the cached sidebar. */
export function browserPreferences(id: string): Preferences {
  const theme = storedTheme();
  const language = storedLocale();
  const sidebar = cachedSidebar(id);
  return {
    ...(theme !== "system" && { theme }),
    ...(language && { language }),
    ...(sidebar && { sidebar }),
  };
}

/** Applies theme and language to the page and the sidebar to its cache. */
function apply(preferences: Preferences, keys: readonly (keyof Preferences)[]) {
  if (keys.includes("theme")) applyTheme(preferences.theme ?? "system");
  if (keys.includes("language") && preferences.language) applyLocale(preferences.language);
  if (keys.includes("sidebar") && user) cacheSidebar(user, preferences.sidebar);
}

async function send(keepalive = false) {
  if (!user || !state.loaded) return;
  timer = null;
  set({ save: "saving" });
  try {
    await api.setPreferences(state.preferences, { keepalive });
    if (!timer) set({ save: "idle" });
  } catch (e) {
    if (!isAbortError(e)) set({ save: "error" });
  }
}

function schedule() {
  if (timer) clearTimeout(timer);
  timer = setTimeout(() => void send(), SAVE_DELAY);
}

/** Sends a change still waiting, as the page goes away. */
function flush() {
  if (!timer) return;
  clearTimeout(timer);
  void send(true);
}

/**
 * Reads `id`'s preferences from the server and applies them. Until they
 * arrive, the browser's cached ones show. Returns the cleanup that stops
 * the store, for an effect.
 */
export function startPreferences(id: string): () => void {
  stopPreferences();
  user = id;
  changedEarly = new Set();
  const local = browserPreferences(id);
  set({ preferences: local, loaded: false, save: "idle" });
  const ours = new AbortController();
  controller = ours;
  api.preferences(ours.signal).then(
    (server) => {
      if (controller !== ours) return;
      // The server wins, except for what the browser has and the server
      // does not (the first time), and what changed meanwhile.
      const migrated = (["theme", "language"] as const).filter((key) => server[key] === undefined && local[key] !== undefined);
      const merged: Preferences = { ...server };
      for (const key of [...migrated, ...changedEarly]) {
        if (state.preferences[key] === undefined) delete merged[key];
        else Object.assign(merged, { [key]: state.preferences[key] });
      }
      set({ preferences: merged, loaded: true });
      apply(merged, ["theme", "language", "sidebar"]);
      if (migrated.length > 0 || changedEarly.size > 0) void send();
    },
    () => {
      // Offline or refused: the cached preferences stay, and nothing is
      // sent, so a server document is never overwritten unread.
    },
  );
  window.addEventListener("pagehide", flush);
  return stopPreferences;
}

/** Stops the store: sends a change still waiting, forgets the person. */
export function stopPreferences() {
  flush();
  controller?.abort();
  controller = null;
  if (timer) clearTimeout(timer);
  timer = null;
  user = null;
  if (typeof window !== "undefined") window.removeEventListener("pagehide", flush);
  state = EMPTY;
  listeners.forEach((listener) => listener());
}

/** Sets one preference, `undefined` for the default: applied now, saved shortly. */
export function setPreference<K extends keyof Preferences>(key: K, value: Preferences[K]) {
  const preferences = { ...state.preferences };
  if (value === undefined) delete preferences[key];
  else preferences[key] = value;
  if (!state.loaded) changedEarly.add(key);
  set({ preferences });
  apply(preferences, [key]);
  if (state.loaded) schedule();
}

/** Sets the theme, through the store when someone is signed in. */
export function setTheme(theme: Preferences["theme"] & string) {
  // "system" is stored too: it is a choice, which a browser's older one must not replace.
  if (user) setPreference("theme", theme);
  else applyTheme(theme);
}

/** Sets the language, through the store when someone is signed in. */
export function setLanguage(language: Locale) {
  if (user) setPreference("language", language);
  else applyLocale(language);
}

const snapshot = () => state;
const serverSnapshot = () => EMPTY;

/** The preferences, whether they are loaded, and how saving went. */
export function usePreferences(): State {
  return useSyncExternalStore(subscribe, snapshot, serverSnapshot);
}
