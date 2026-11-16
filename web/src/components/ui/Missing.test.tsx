import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import { Missing, missingText } from "./Missing";

describe("Missing", () => {
  it("says why a value is missing, in the active language", () => {
    render(<Missing word="never" className="dim" />);
    expect(screen.getByText("Never")).toHaveClass("dim");
    setActiveLocale("fr");
    expect(missingText("unknown")).toBe("Inconnu");
    expect(missingText("notYet")).toBe("Pas encore");
  });
});
