import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { describe, expect, it, vi } from "vitest";

import { Switch } from "./Switch";

function Controlled({ initial = false, onChange = vi.fn() }: { initial?: boolean; onChange?: (v: boolean) => void }) {
  const [on, setOn] = useState(initial);
  return (
    <Switch
      checked={on}
      onChange={(v) => {
        setOn(v);
        onChange(v);
      }}
      label="Allow reboot"
      note="(when an update needs it)"
    />
  );
}

describe("Switch", () => {
  it("is a switch named by its label, with aria-checked", () => {
    render(<Controlled initial />);
    const sw = screen.getByRole("switch", { name: /Allow reboot/ });
    expect(sw).toHaveAttribute("aria-checked", "true");
    expect(sw).toBeChecked();
    expect(screen.getByText("(when an update needs it)")).toBeInTheDocument();
  });

  it("toggles on a click on the switch or on its words", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<Controlled onChange={onChange} />);
    const sw = screen.getByRole("switch", { name: /Allow reboot/ });
    await user.click(sw);
    expect(sw).toBeChecked();
    await user.click(screen.getByText("Allow reboot"));
    expect(sw).not.toBeChecked();
    expect(onChange.mock.calls).toEqual([[true], [false]]);
  });

  it("toggles with Space and Enter", async () => {
    const user = userEvent.setup();
    render(<Controlled />);
    const sw = screen.getByRole("switch", { name: /Allow reboot/ });
    sw.focus();
    await user.keyboard(" ");
    expect(sw).toBeChecked();
    await user.keyboard("{Enter}");
    expect(sw).not.toBeChecked();
  });

  it("does nothing when disabled", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(<Switch checked disabled onChange={onChange} label="Settings" />);
    await user.click(screen.getByRole("switch", { name: "Settings" }));
    expect(onChange).not.toHaveBeenCalled();
  });

  it("can stand alone with an aria-label", () => {
    render(<Switch checked={false} onChange={() => {}} aria-label="Show Fleet" />);
    expect(screen.getByRole("switch", { name: "Show Fleet" })).not.toBeChecked();
  });
});
