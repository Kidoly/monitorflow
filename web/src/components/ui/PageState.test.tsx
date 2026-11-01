import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ErrorState, NoPermission } from "./PageState";

describe("page states", () => {
  it("retries from the empty box of a failed first load", async () => {
    const retry = vi.fn();
    render(<ErrorState what="servers" error="Timed out." onRetry={retry} />);
    await userEvent.click(screen.getByRole("button", { name: "Retry now" }));
    expect(retry).toHaveBeenCalledOnce();
  });

  it("leads back from a page the roles do not open", () => {
    render(<NoPermission title="Mirrors need a global administrator" />);
    expect(screen.getByRole("link", { name: "Back to the fleet" })).toHaveAttribute("href", "/");
  });
});
