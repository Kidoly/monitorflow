import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setUrl } from "@/test/navigation";

import { FilterBar } from "./FilterBar";

vi.mock("next/navigation", () => import("@/test/navigation"));

const query = () => Object.fromEntries(new URLSearchParams(window.location.search));

function renderBar(onChange = vi.fn()) {
  render(
    <FilterBar
      filters={[
        {
          name: "status",
          label: "Status",
          options: [
            { value: "online", label: "Online" },
            { value: "offline", label: "Offline" },
          ],
        },
        { kind: "toggle", name: "incidents", label: "Has incidents" },
      ]}
      search={{ name: "q", label: "Search", placeholder: "Search hosts…" }}
      onChange={onChange}
      trailing={<button type="button">Problems first</button>}
    />,
  );
  return onChange;
}

describe("FilterBar", () => {
  beforeEach(() => setUrl("/?customer=acme"));

  it("writes each change into the URL with replaceState and reports it", async () => {
    const user = userEvent.setup();
    const replaceState = vi.spyOn(window.history, "replaceState");
    const pushState = vi.spyOn(window.history, "pushState");
    const onChange = renderBar();

    await user.selectOptions(screen.getByRole("combobox", { name: "Status" }), "offline");
    expect(query()).toEqual({ customer: "acme", status: "offline" });
    expect(onChange).toHaveBeenLastCalledWith(new URLSearchParams("customer=acme&status=offline"));

    await user.click(screen.getByRole("button", { name: "Has incidents" }));
    expect(query()).toEqual({ customer: "acme", status: "offline", incidents: "1" });
    expect(screen.getByRole("button", { name: "Has incidents" })).toHaveAttribute("aria-pressed", "true");

    await user.type(screen.getByRole("searchbox", { name: "Search" }), "web");
    expect(query()).toEqual({ customer: "acme", status: "offline", incidents: "1", q: "web" });

    expect(replaceState).toHaveBeenCalled();
    expect(pushState).not.toHaveBeenCalled();
  });

  it("reads its state from the URL", () => {
    setUrl("/?status=online&incidents=1&q=db");
    renderBar();
    expect(screen.getByRole("combobox", { name: "Status" })).toHaveValue("online");
    expect(screen.getByRole("option", { name: "Status: Online" })).toHaveProperty("selected", true);
    expect(screen.getByRole("button", { name: "Has incidents" })).toHaveAttribute("aria-pressed", "true");
    expect(screen.getByRole("searchbox", { name: "Search" })).toHaveValue("db");
  });

  it("ignores unknown values", () => {
    setUrl("/?status=exploded");
    renderBar();
    expect(screen.getByRole("combobox", { name: "Status" })).toHaveValue("");
  });

  it("clears its own keys only, keeping the scope and page keys", async () => {
    const user = userEvent.setup();
    setUrl("/servers/view/?id=s1&customer=acme&status=online&q=db&incidents=1");
    renderBar();
    await user.click(screen.getByRole("button", { name: "Clear" }));
    expect(query()).toEqual({ id: "s1", customer: "acme" });
    expect(window.location.pathname).toBe("/servers/view/");
    expect(screen.getByRole("searchbox", { name: "Search" })).toHaveValue("");
    expect(screen.queryByRole("button", { name: "Clear" })).not.toBeInTheDocument();
  });
});
