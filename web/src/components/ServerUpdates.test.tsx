import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { PackageList } from "@/lib/api";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { ServerUpdates } from "./ServerUpdates";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "0b7c5d6e-1f2a-4b3c-8d4e-5f6a7b8c9d0e";
const PATH = `/api/servers/${ID}/packages?limit=5000`;

function report(overrides: Partial<PackageList> = {}): PackageList {
  return {
    reboot_required: true,
    reported_at: new Date(Date.now() - 3_600_000).toISOString(),
    packages: [
      { name: "curl", arch: "amd64", version: "7.88.1-10+deb12u7", upgradable_version: "7.88.1-10+deb12u8" },
      { name: "bash", arch: "amd64", version: "5.2.15-2+b7", upgradable_version: null },
      { name: "openssl", arch: "amd64", version: "3.0.14-1", upgradable_version: "3.0.15-1~deb12u1" },
    ],
    next_cursor: null,
    ...overrides,
  };
}

describe("ServerUpdates", () => {
  beforeEach(() => setUrl(`/servers/view/?id=${ID}&tab=updates`));

  it("counts packages by status and lists upgradable ones first", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: report() } });
    render(<ServerUpdates serverId={ID} />);

    const table = await screen.findByRole("table", { name: "Updates" });
    const names = within(table)
      .getAllByRole("row")
      .slice(1)
      .map((row) => row.querySelector("b")?.textContent);
    expect(names).toEqual(["curl", "openssl", "bash"]);
    expect(within(table).getByText("curl").closest("tr")).toHaveTextContent("7.88.1-10+deb12u7 → 7.88.1-10+deb12u8");
    expect(within(table).getByText("bash").closest("tr")).toHaveTextContent("Up to date");

    const tiles = screen.getByRole("group", { name: "Updates by status, click to filter" });
    const upgradable = within(tiles).getByRole("link", { name: /Upgradable/ });
    expect(upgradable).toHaveTextContent("Upgradable2");
    expect(upgradable.getAttribute("href")).toMatch(new RegExp(`^/servers/view/?\\?id=${ID}&tab=updates&updates=upgradable$`));
    expect(within(tiles).getByRole("link", { name: /Installed/ })).toHaveAttribute("aria-current", "true");
    expect(screen.getByText("Reboot required")).toBeInTheDocument();
    expect(screen.getByText(/3 packages installed/)).toBeInTheDocument();
  });

  it("filters by status and name in the URL, keeping the page's own keys", async () => {
    const user = userEvent.setup();
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: report() } });
    render(<ServerUpdates serverId={ID} />);

    await user.selectOptions(await screen.findByRole("combobox", { name: "Status" }), "upgradable");
    expect(window.location.search).toBe(`?id=${ID}&tab=updates&updates=upgradable`);
    expect(screen.queryByText("bash")).not.toBeInTheDocument();

    await user.type(screen.getByRole("searchbox", { name: "Search packages" }), "ssl");
    const table = screen.getByRole("table", { name: "Updates" });
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    expect(within(table).getByText("openssl")).toBeInTheDocument();
  });

  it("follows the report's pages", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: { status: 200, body: report({ packages: report().packages.slice(0, 1), next_cursor: "c1" }) },
      [`GET ${PATH}&cursor=c1`]: { status: 200, body: report({ packages: report().packages.slice(1) }) },
    });
    render(<ServerUpdates serverId={ID} />);
    expect(await screen.findByText(/3 packages installed/)).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });

  it("says when the agent has not reported yet", async () => {
    stubFetch({
      [`GET ${PATH}`]: { status: 200, body: report({ reported_at: null, reboot_required: null, packages: [] }) },
    });
    render(<ServerUpdates serverId={ID} />);
    expect(await screen.findByText("No package report yet")).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("shows the error of a first load", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 500, body: { error: "internal" } } });
    render(<ServerUpdates serverId={ID} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load the package report");
  });
});
