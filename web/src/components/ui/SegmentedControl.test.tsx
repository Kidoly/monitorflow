import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { SegmentedControl } from "./SegmentedControl";

const OPTIONS = [
  { value: "system", label: "Follow the system" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark", disabled: false },
] as const;

type Theme = (typeof OPTIONS)[number]["value"];

function Controlled({ onChange = vi.fn(), disabledDark = false }: { onChange?: (v: Theme) => void; disabledDark?: boolean }) {
  const [value, setValue] = useState<Theme>("system");
  return (
    <SegmentedControl
      label="Theme"
      options={OPTIONS.map((o) => ({ ...o, disabled: o.value === "dark" && disabledDark }))}
      value={value}
      onChange={(v) => {
        setValue(v);
        onChange(v);
      }}
    />
  );
}

describe("SegmentedControl", () => {
  it("is a named radio group with the chosen option checked and the only tab stop", () => {
    render(<Controlled />);
    expect(screen.getByRole("radiogroup", { name: "Theme" })).toBeInTheDocument();
    const system = screen.getByRole("radio", { name: "Follow the system" });
    expect(system).toBeChecked();
    expect(system).toHaveAttribute("tabindex", "0");
    expect(screen.getByRole("radio", { name: "Light" })).toHaveAttribute("tabindex", "-1");
  });

  it("chooses on click", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<Controlled onChange={onChange} />);
    await user.click(screen.getByRole("radio", { name: "Light" }));
    expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Follow the system" })).not.toBeChecked();
    // Clicking the chosen option again does not report a change.
    await user.click(screen.getByRole("radio", { name: "Light" }));
    expect(onChange.mock.calls).toEqual([["light"]]);
  });

  it("moves and chooses with the arrow keys, Home and End, skipping disabled options", async () => {
    const user = userEvent.setup();
    render(<Controlled disabledDark />);
    screen.getByRole("radio", { name: "Follow the system" }).focus();
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
    expect(screen.getByRole("radio", { name: "Light" })).toHaveFocus();
    // Dark is disabled: the next step wraps to the first option.
    await user.keyboard("{ArrowRight}");
    expect(screen.getByRole("radio", { name: "Follow the system" })).toBeChecked();
    await user.keyboard("{End}");
    expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
    await user.keyboard("{Home}");
    expect(screen.getByRole("radio", { name: "Follow the system" })).toHaveFocus();
    await user.keyboard("{ArrowLeft}");
    expect(screen.getByRole("radio", { name: "Light" })).toBeChecked();
  });
});
