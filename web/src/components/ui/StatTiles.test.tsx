import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { StatTiles } from "./StatTiles";

describe("StatTiles", () => {
  it("links each counter to its filter", () => {
    render(
      <StatTiles
        tiles={[
          { label: "Online", value: 13, total: 17, tone: "ok", href: "/?status=online" },
          { label: "Critical incidents", value: 2, tone: "bad", href: "/?severity=critical", active: true },
          { label: "Peak", value: "94.6 %" },
        ]}
      />,
    );
    const group = screen.getByRole("group", { name: "Summary, click to filter" });
    const online = within(group).getByRole("link", { name: /Online/ });
    expect(online).toHaveAttribute("href", "/?status=online");
    expect(online).toHaveTextContent("Online13 / 17");
    expect(within(group).getByRole("link", { name: /Critical/ })).toHaveAttribute("aria-current", "true");
    // Without href it is a fact, not a link.
    expect(within(group).getAllByRole("link")).toHaveLength(2);
    expect(within(group).getByText("94.6 %")).toBeInTheDocument();
  });
});
