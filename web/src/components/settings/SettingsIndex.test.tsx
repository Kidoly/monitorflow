import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { Me } from "@/lib/api";
import { As, customerAdmin, me, viewer } from "@/test/admin";
import { setUrl } from "@/test/navigation";

import { SettingsIndex } from "./SettingsIndex";

vi.mock("next/navigation", () => import("@/test/navigation"));

const global = me({ role: "administrator", customer_id: null, site_id: null });

function open(user: Me) {
  render(
    <As user={user}>
      <SettingsIndex />
    </As>,
  );
}

const rowNames = () => screen.queryAllByRole("link").map((link) => link.querySelector("span span")?.textContent);

describe("SettingsIndex", () => {
  beforeEach(() => setUrl("/settings/"));

  it("lists every page in groups, as rows linking to them", () => {
    open(global);
    expect(screen.getAllByRole("heading", { level: 2 }).map((h) => h.textContent)).toEqual([
      "Fleet",
      "Alerting",
      "Access",
      "AI & extensions",
      "System",
    ]);
    const fleet = screen.getByRole("region", { name: "Fleet" });
    expect(within(fleet).getByRole("link", { name: /Add servers/ })).toHaveAttribute("href", expect.stringMatching(/^\/settings\/add-servers\/?$/));
    expect(screen.getByRole("link", { name: /General/ })).toHaveAttribute("href", expect.stringMatching(/^\/settings\/general\/?$/));
  });

  it("filters as one types, and says when nothing matches", async () => {
    const user = userEvent.setup();
    open(global);
    const search = screen.getByRole("searchbox", { name: "Search settings" });
    await user.type(search, "backup");
    expect(rowNames()).toEqual(["Backups"]);
    await user.clear(search);
    await user.type(search, "webhooks");
    expect(rowNames()).toEqual(["Notifications"]);
    await user.type(search, " zebra");
    expect(screen.getByText("No settings match “webhooks zebra”.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Clear search" }));
    expect(rowNames()).toHaveLength(14);
  });

  it("clears the search with Escape", async () => {
    const user = userEvent.setup();
    open(global);
    const search = screen.getByRole("searchbox", { name: "Search settings" });
    await user.type(search, "relay{Escape}");
    expect(search).toHaveValue("");
  });

  it("shows customer administrators their pages only, and others a note", () => {
    open(customerAdmin);
    expect(rowNames()).toEqual(["Sites", "Relays", "Add servers", "Notifications", "Integrations", "Users and roles", "API keys"]);
  });

  it("tells other roles they need an administrator", () => {
    open(viewer);
    expect(screen.getByText("Settings need an administrator role.")).toBeInTheDocument();
  });
});
