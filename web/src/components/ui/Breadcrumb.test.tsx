import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { Breadcrumb, type Sibling } from "./Breadcrumb";

const servers: Sibling[] = ["web-1", "web-2", "db-1"].map((name) => ({ label: name, href: `/servers/view/?id=${name}`, current: name === "web-1" }));

function crumbs(load = vi.fn(async () => servers)) {
  render(
    <>
      <Breadcrumb parents={[{ label: "Fleet", href: "/" }]} current="web-1" siblings={{ label: "Other servers of paris", load }} />
      <button type="button">Elsewhere</button>
    </>,
  );
  return load;
}

describe("Breadcrumb", () => {
  it("lists the siblings under ▾, loaded once, the current one not a link", async () => {
    const user = userEvent.setup();
    const load = crumbs();
    const toggle = screen.getByRole("button", { name: "Other servers of paris" });
    expect(toggle).toHaveAttribute("aria-expanded", "false");
    await user.click(toggle);
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    const list = await screen.findByRole("list", { name: "Other servers of paris" });
    expect(screen.getByRole("link", { name: "web-2" })).toHaveAttribute("href", expect.stringMatching(/id=web-2$/));
    expect(screen.queryByRole("link", { name: "web-1" })).toBeNull();
    expect(list).toHaveTextContent("web-1web-2db-1");
    // Focus goes to the first link; arrows move, Escape closes and returns.
    expect(screen.getByRole("link", { name: "web-2" })).toHaveFocus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("link", { name: "db-1" })).toHaveFocus();
    await user.keyboard("{Escape}");
    expect(screen.queryByRole("list")).toBeNull();
    expect(toggle).toHaveFocus();
    await user.click(toggle);
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("closes on a click elsewhere, and the click still lands", async () => {
    const user = userEvent.setup();
    crumbs();
    await user.click(screen.getByRole("button", { name: "Other servers of paris" }));
    await screen.findByRole("list");
    const elsewhere = screen.getByRole("button", { name: "Elsewhere" });
    await user.click(elsewhere);
    expect(screen.queryByRole("list")).toBeNull();
    expect(elsewhere).toHaveFocus();
  });

  it("filters a long list", async () => {
    const user = userEvent.setup();
    const many = Array.from({ length: 12 }, (_, i) => ({ label: `web-${i + 1}`, href: `/s/${i}` }));
    crumbs(vi.fn(async () => many));
    await user.click(screen.getByRole("button", { name: "Other servers of paris" }));
    const filter = await screen.findByRole("searchbox", { name: "Filter: Other servers of paris" });
    expect(filter).toHaveFocus();
    await user.type(filter, "web-1");
    const list = screen.getByRole("list", { name: "Other servers of paris" });
    expect(within(list).getAllByRole("link").map((l) => l.textContent)).toEqual(["web-1", "web-10", "web-11", "web-12"]);
  });

  it("says when the list cannot load", async () => {
    const user = userEvent.setup();
    vi.spyOn(console, "warn").mockImplementation(() => {});
    crumbs(vi.fn(async () => Promise.reject(new Error("down"))));
    await user.click(screen.getByRole("button", { name: "Other servers of paris" }));
    expect(await screen.findByText("Could not load the list.")).toBeInTheDocument();
  });
});
