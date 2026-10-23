import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { InlineConfirm } from "./InlineConfirm";

const REASONS = [
  { value: "decommissioned", label: "Decommissioned" },
  { value: "key_compromise", label: "Key compromised" },
];

function renderConfirm(onConfirm = vi.fn(async () => {})) {
  render(
    <InlineConfirm
      trigger="Revoke web-01…"
      title="Revoke web-01?"
      consequence="The agent will be disconnected and its certificate rejected. This cannot be undone."
      reasons={REASONS}
      confirmLabel="Revoke web-01"
      pendingLabel="Revoking…"
      onConfirm={onConfirm}
      describeError={() => "Revocation failed."}
    />,
  );
  return onConfirm;
}

describe("InlineConfirm", () => {
  it("opens in place, focuses the reason and keeps the danger button disabled until one is chosen", async () => {
    const user = userEvent.setup();
    const onConfirm = renderConfirm();
    await user.click(screen.getByRole("button", { name: "Revoke web-01…" }));
    expect(screen.queryByRole("button", { name: "Revoke web-01…" })).not.toBeInTheDocument();
    expect(screen.queryByRole("dialog")).not.toBeInTheDocument();
    const form = screen.getByRole("form", { name: "Revoke web-01?" });
    expect(form).toHaveTextContent("Revoke web-01? The agent will be disconnected");
    const reason = screen.getByRole("combobox", { name: "Reason" });
    expect(reason).toHaveFocus();
    const confirm = screen.getByRole("button", { name: "Revoke web-01" });
    expect(confirm).toBeDisabled();

    await user.selectOptions(reason, "key_compromise");
    expect(confirm).toBeEnabled();
    await user.click(confirm);
    expect(onConfirm).toHaveBeenCalledWith("key_compromise");
    expect(screen.queryByRole("form")).not.toBeInTheDocument();
  });

  it("gives focus back to the trigger on cancel", async () => {
    const user = userEvent.setup();
    const onConfirm = renderConfirm();
    await user.click(screen.getByRole("button", { name: "Revoke web-01…" }));
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(screen.getByRole("button", { name: "Revoke web-01…" })).toHaveFocus();
    expect(onConfirm).not.toHaveBeenCalled();
    // A new opening starts without the earlier reason.
    await user.click(screen.getByRole("button", { name: "Revoke web-01…" }));
    expect(screen.getByRole("button", { name: "Revoke web-01" })).toBeDisabled();
  });

  it("disables everything while working and stays open on failure", async () => {
    const user = userEvent.setup();
    let fail: (e: Error) => void = () => {};
    renderConfirm(vi.fn(() => new Promise<void>((_, reject) => (fail = reject))));
    await user.click(screen.getByRole("button", { name: "Revoke web-01…" }));
    await user.selectOptions(screen.getByRole("combobox", { name: "Reason" }), "decommissioned");
    await user.click(screen.getByRole("button", { name: "Revoke web-01" }));
    expect(screen.getByRole("button", { name: "Revoking…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Cancel" })).toBeDisabled();
    fail(new Error("boom"));
    expect(await screen.findByRole("alert")).toHaveTextContent("Revocation failed.");
    expect(screen.getByRole("button", { name: "Revoke web-01" })).toBeEnabled();
  });

  it("focuses the danger button when the action takes no reason", async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    render(<InlineConfirm trigger="Revoke token…" title="Revoke this token?" consequence="It stops working." confirmLabel="Revoke token" onConfirm={onConfirm} />);
    await user.click(screen.getByRole("button", { name: "Revoke token…" }));
    expect(screen.getByRole("button", { name: "Revoke token" })).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(onConfirm).toHaveBeenCalledWith(null);
  });
});
