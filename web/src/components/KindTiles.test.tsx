import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { KindTiles } from "./KindTiles";

describe("KindTiles", () => {
  it("is a radio group of named pictures", async () => {
    const onChange = vi.fn();
    render(<KindTiles label="Draw as" kinds={["chart", "gauge", "states"]} value="gauge" onChange={onChange} />);
    const group = screen.getByRole("radiogroup", { name: "Draw as" });
    expect(group.querySelectorAll("svg")).toHaveLength(3);
    expect(screen.getByRole("radio", { name: "Gauge" })).toBeChecked();
    await userEvent.click(screen.getByRole("radio", { name: "State grid" }));
    expect(onChange).toHaveBeenCalledWith("states");
  });
});
