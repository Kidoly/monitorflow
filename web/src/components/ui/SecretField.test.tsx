import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { COPIED_MS, CopyButton, SecretField } from "./SecretField";

const TOKEN = "mft_9Qk2Zr7wLx3pV8hN4sT6uB1cE0fG5jM2aD7yR";

describe("SecretField", () => {
  afterEach(() => vi.useRealTimers());

  it("copies the secret, reads Copied for 2 s and announces it", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const writeText = vi.spyOn(navigator.clipboard, "writeText").mockResolvedValue();
    render(<SecretField label="Enrollment token — shown once" value={TOKEN} />);
    const input = screen.getByRole("textbox", { name: "Enrollment token — shown once" });
    expect(input).toHaveValue(TOKEN);
    expect(input).toHaveAttribute("readonly");

    await user.click(screen.getByRole("button", { name: "Copy" }));
    expect(writeText).toHaveBeenCalledWith(TOKEN);
    expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument();
    const live = document.querySelector("[aria-live=polite]");
    expect(live).toHaveTextContent("Copied");

    await act(() => vi.advanceTimersByTimeAsync(COPIED_MS));
    expect(screen.getByRole("button", { name: "Copy" })).toBeInTheDocument();
    expect(live).toHaveTextContent("");
  });

  it("says when copying is refused", async () => {
    // setup() installs its clipboard stub; spy on it afterwards.
    const user = userEvent.setup();
    vi.spyOn(navigator.clipboard, "writeText").mockRejectedValue(new Error("denied"));
    render(<SecretField label="Token" value={TOKEN} />);
    await user.click(screen.getByRole("button", { name: "Copy" }));
    expect(document.querySelector("[aria-live=polite]")).toHaveTextContent("Copying failed; select the text instead.");
  });

  it("names the Copy button when asked, and copies any text with CopyButton", async () => {
    const user = userEvent.setup();
    render(
      <>
        <SecretField label="Token" value={TOKEN} copyName="Copy token" />
        <CopyButton text="token = 1" name="Copy configuration snippet" size="small" />
      </>,
    );
    await user.click(screen.getByRole("button", { name: "Copy token" }));
    expect(await navigator.clipboard.readText()).toBe(TOKEN);
    await user.click(screen.getByRole("button", { name: "Copy configuration snippet" }));
    expect(await navigator.clipboard.readText()).toBe("token = 1");
    expect(screen.getByRole("button", { name: "Copy configuration snippet" })).toHaveTextContent("Copied");
  });
});

