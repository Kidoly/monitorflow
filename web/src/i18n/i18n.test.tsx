import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { rich, t } from ".";
import { setActiveLocale } from "./locale";

describe("t", () => {
  it("fills placeholders, formatting numbers for the language", () => {
    expect(t("shell.scope.current", { name: "Acme" })).toBe("Scope: Acme");
    expect(t("shell.settings.attention", { count: 1234 })).toBe("1,234 settings need attention");
    setActiveLocale("fr");
    expect(t("shell.scope.current", { name: "Acme" })).toBe("Périmètre : Acme");
    expect(t("shell.settings.attention", { count: 1234 })).toBe("1\u202f234 paramètres demandent votre attention");
  });

  it("picks the plural form of the language", () => {
    expect(t("shell.settings.attention", { count: 0 })).toBe("0 settings need attention");
    expect(t("shell.settings.attention", { count: 1 })).toBe("1 setting needs attention");
    setActiveLocale("fr");
    // French counts zero as one.
    expect(t("shell.settings.attention", { count: 0 })).toBe("0 paramètre demande votre attention");
    expect(t("shell.settings.attention", { count: 2 })).toBe("2 paramètres demandent votre attention");
  });

  it("falls back to the key for a message it does not know", () => {
    expect(t("shell.nope" as "shell.menu")).toBe("shell.nope");
  });
});

describe("rich", () => {
  it("puts elements in the sentence", () => {
    render(<p>{rich("shell.update.running", { version: <strong>0.4.0</strong> })}</p>);
    expect(screen.getByText("0.4.0").tagName).toBe("STRONG");
    expect(screen.getByText(/This server runs/)).toHaveTextContent("This server runs 0.4.0.");
  });
});
