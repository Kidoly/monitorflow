import { act, render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setLanguage } from "@/lib/preferences";

import { t } from ".";
import { LOCALE_KEY, applyLocale, matchLocale, preferredLocale } from "./locale";
import { LocaleProvider } from "./LocaleProvider";

const languages = (list: string[]) => vi.spyOn(navigator, "languages", "get").mockReturnValue(list);

function Sample() {
  return <p>{t("shell.signOut")}</p>;
}

describe("locale", () => {
  beforeEach(() => {
    localStorage.clear();
    document.documentElement.lang = "en";
  });

  it("matches the browser's languages by their first part", () => {
    expect(matchLocale(["de-DE", "fr-CA", "en"])).toBe("fr");
    expect(matchLocale(["EN-gb"])).toBe("en");
    expect(matchLocale(["de", "es"])).toBeNull();
  });

  it("prefers the stored choice, then the browser's language, then English", () => {
    languages(["de-DE"]);
    expect(preferredLocale()).toBe("en");
    languages(["fr-FR", "en"]);
    expect(preferredLocale()).toBe("fr");
    localStorage.setItem(LOCALE_KEY, "en");
    expect(preferredLocale()).toBe("en");
    localStorage.setItem(LOCALE_KEY, "xx");
    expect(preferredLocale()).toBe("fr");
  });

  it("draws the page in the browser's language and sets <html lang>", () => {
    languages(["fr-BE"]);
    render(
      <LocaleProvider>
        <Sample />
      </LocaleProvider>,
    );
    expect(screen.getByText("Se déconnecter")).toBeInTheDocument();
    expect(document.documentElement.lang).toBe("fr");
  });

  it("switches language at once and keeps the choice", () => {
    render(
      <LocaleProvider>
        <Sample />
      </LocaleProvider>,
    );
    expect(screen.getByText("Sign out")).toBeInTheDocument();
    act(() => setLanguage("fr"));
    expect(screen.getByText("Se déconnecter")).toBeInTheDocument();
    expect(localStorage.getItem(LOCALE_KEY)).toBe("fr");
    expect(document.documentElement.lang).toBe("fr");
    act(() => applyLocale("en"));
    expect(screen.getByText("Sign out")).toBeInTheDocument();
  });
});
