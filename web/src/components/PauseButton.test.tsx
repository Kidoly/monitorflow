import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { PauseButton, pauseEnd } from "./PauseButton";

describe("pauseEnd", () => {
  const now = new Date(2026, 10, 22, 14, 30);
  it("ends a pause on its own, 30 days at most", () => {
    expect(pauseEnd("hour", now, "")).toEqual(new Date(2026, 10, 22, 15, 30));
    expect(pauseEnd("tomorrow", now, "")).toEqual(new Date(2026, 10, 23, 8));
    expect(pauseEnd("week", now, "")!.getTime() - now.getTime()).toBe(7 * 86_400_000);
    // A few minutes short of 30 days, so a browser clock a little ahead is not refused.
    expect(pauseEnd("max", now, "")!.getTime() - now.getTime()).toBe(30 * 86_400_000 - 300_000);
    expect(pauseEnd("until", now, "2026-11-25T09:00")).toEqual(new Date(2026, 10, 25, 9));
    expect(pauseEnd("until", now, "")).toBeNull();
  });
});

describe("PauseButton", () => {
  it("asks why and for how long in place, and gives focus back on Escape", async () => {
    const user = userEvent.setup();
    const onPause = vi.fn(async () => {});
    render(<PauseButton name="Pause the 2 selected sensors" onPause={onPause} describeError={() => "failed"} />);
    const trigger = screen.getByRole("button", { name: "Pause the 2 selected sensors" });
    await user.click(trigger);
    await user.keyboard("{Escape}");
    expect(screen.getByRole("button", { name: "Pause the 2 selected sensors" })).toHaveFocus();

    await user.click(screen.getByRole("button", { name: "Pause the 2 selected sensors" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "For" }), "Until a date…");
    await user.type(screen.getByRole("textbox", { name: "Why" }), "migration");
    await user.type(screen.getByLabelText(/^Until/), "2099-01-01T08:00");
    await user.click(screen.getByRole("button", { name: "Pause" }));
    expect(onPause).toHaveBeenCalledWith({ reason: "migration", ends_at: new Date(2099, 0, 1, 8).toISOString() });
    expect(screen.queryByRole("form")).not.toBeInTheDocument();
  });

  it("keeps the form open with the API's error", async () => {
    const user = userEvent.setup();
    render(<PauseButton name="Pause" onPause={() => Promise.reject(new Error("no"))} describeError={() => "A silence lasts at most 30 days."} />);
    await user.click(screen.getByRole("button", { name: "Pause" }));
    await user.type(screen.getByRole("textbox", { name: "Why" }), "x{Enter}");
    expect(await screen.findByRole("alert")).toHaveTextContent("A silence lasts at most 30 days.");
  });
});
