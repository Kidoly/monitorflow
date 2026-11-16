import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Hint } from "./Hint";

describe("Hint", () => {
  it("is a focusable badge described by its explanation", () => {
    render(
      <Hint tone="warn" label="Rollback limited">
        No snapshot support on this server.
      </Hint>,
    );
    const badge = screen.getByText("Rollback limited").closest("[tabindex]")!;
    expect(badge).toHaveAttribute("tabindex", "0");
    expect(badge).toHaveAccessibleDescription("No snapshot support on this server.");
    expect(screen.getByRole("tooltip", { hidden: true })).toHaveTextContent("No snapshot support on this server.");
  });
});
