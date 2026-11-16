import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { afterEach, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  // Render tests read English unless they pick a language.
  setActiveLocale("en");
});
