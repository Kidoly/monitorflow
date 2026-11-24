import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { InventorySearchPage, queryFromParams } from "./InventorySearch";

vi.mock("next/navigation", () => import("@/test/navigation"));

const hit = {
  server_id: "s1",
  hostname: "web-1",
  site_id: "site-1",
  site: "paris",
  source: "package",
  name: "openssl",
  version: "3.0.14-1~deb12u2",
  fields: { arch: "amd64" },
};

describe("InventorySearchPage", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    window.history.replaceState(null, "", "/inventory/");
  });

  it("reads the search from the URL", () => {
    expect(queryFromParams(new URLSearchParams("kind=disk&q=wdc&exact=1&below=2&from=1"))).toEqual({
      kind: "disk",
      q: "wdc",
      exact: true,
      below: "2",
      atLeast: "1",
    });
    expect(queryFromParams(new URLSearchParams("kind=gadget")).kind).toBe("software");
  });

  it("searches software by name and version, and links the CSV", async () => {
    const fetchMock = stubFetch({
      "GET /api/inventory?kind=software&q=openssl&version_below=3.0.15": { status: 200, body: { hits: [hit], truncated: false } },
    });
    render(
      <As user={viewer}>
        <InventorySearchPage />
      </As>,
    );
    expect(screen.getByText("Choose what to look for, then search.")).toBeTruthy();
    const search = screen.getByRole("button", { name: "Search" });
    expect(search.hasAttribute("disabled")).toBe(true);
    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox"), "openssl");
    await user.type(screen.getByRole("textbox", { name: "Version below" }), "3.0.15");
    await user.click(search);
    const table = await screen.findByRole("table", { name: "Inventory" });
    expect(table.textContent).toContain("web-1");
    expect(table.textContent).toContain("3.0.14-1~deb12u2");
    expect(screen.getByRole("link", { name: "web-1" }).getAttribute("href")).toContain("?id=s1&tab=inventory");
    expect(screen.getByRole("link", { name: "Export CSV" }).getAttribute("href")).toBe(
      "/api/inventory?kind=software&q=openssl&version_below=3.0.15&format=csv",
    );
    await waitFor(() => expect(fetchMock).toHaveBeenCalledTimes(1));
    expect(window.location.search).toBe("?kind=software&q=openssl&below=3.0.15");
  });

  it("lists every disk without text, and says when nothing matches", async () => {
    stubFetch({ "GET /api/inventory?kind=disk": { status: 200, body: { hits: [], truncated: false } } });
    window.history.replaceState(null, "", "/inventory/?kind=disk");
    render(
      <As user={viewer}>
        <InventorySearchPage />
      </As>,
    );
    expect(await screen.findByText("Nothing matches.")).toBeTruthy();
  });
});
