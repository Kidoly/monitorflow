import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { InlineConfirm } from "./InlineConfirm";
import { RowMenu } from "./RowMenu";

describe("RowMenu", () => {
  function open() {
    const onSelect = vi.fn();
    render(
      <>
        <RowMenu
          label="Actions for web-1"
          items={[
            { label: "Open chart", href: "/servers/sensor/?id=x" },
            { label: "Turn off…", onSelect, danger: true },
          ]}
        />
        <p>elsewhere</p>
      </>,
    );
    return { button: screen.getByRole("button", { name: "Actions for web-1" }), onSelect };
  }

  it("opens from the keyboard on the first item and moves with the arrows", async () => {
    const user = userEvent.setup();
    const { button } = open();
    expect(button).toHaveAttribute("aria-haspopup", "menu");
    expect(button).toHaveAttribute("aria-expanded", "false");
    button.focus();
    await user.keyboard("{ArrowDown}");
    expect(button).toHaveAttribute("aria-expanded", "true");
    const items = within(screen.getByRole("menu", { name: "Actions for web-1" })).getAllByRole("menuitem");
    expect(items[0]).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(items[1]).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(items[0]).toHaveFocus();
    await user.keyboard("{End}");
    expect(items[1]).toHaveFocus();
  });

  it("closes on Escape with focus back on its button, and on a click elsewhere", async () => {
    const user = userEvent.setup();
    const { button } = open();
    await user.click(button);
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
    expect(button).toHaveFocus();
    await user.click(button);
    await user.click(screen.getByText("elsewhere"));
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });

  it("runs a choice and closes", async () => {
    const user = userEvent.setup();
    const { button, onSelect } = open();
    await user.click(button);
    await user.click(screen.getByRole("menuitem", { name: "Turn off…" }));
    expect(onSelect).toHaveBeenCalledOnce();
    expect(screen.queryByRole("menu")).not.toBeInTheDocument();
  });
});

describe("InlineConfirm opened from elsewhere", () => {
  it("opens without a trigger, focused on its button, and says when it closes", async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    const onConfirm = vi.fn();
    const { rerender } = render(
      <InlineConfirm layout="row" trigger="Turn off…" title="Turn off CPU usage?" consequence="" confirmLabel="Turn off" onConfirm={onConfirm} onClose={onClose} />,
    );
    const form = screen.getByRole("form", { name: "Turn off CPU usage?" });
    expect(within(form).getByRole("button", { name: "Turn off" })).toHaveFocus();
    await user.click(within(form).getByRole("button", { name: "Cancel" }));
    expect(onClose).toHaveBeenCalledOnce();
    expect(onConfirm).not.toHaveBeenCalled();
    expect(screen.queryByRole("button", { name: "Turn off…" })).not.toBeInTheDocument();

    rerender(<p>gone</p>);
    render(
      <InlineConfirm layout="row" trigger="Turn off…" title="Turn off disk?" consequence="" confirmLabel="Turn off" onConfirm={onConfirm} onClose={onClose} />,
    );
    await user.click(screen.getByRole("button", { name: "Turn off" }));
    expect(onConfirm).toHaveBeenCalledOnce();
    expect(onClose).toHaveBeenCalledTimes(2);
  });
});
