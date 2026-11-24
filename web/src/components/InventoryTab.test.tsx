import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ServerDetail } from "@/lib/api";
import type { ServerInventory } from "@/lib/inventory";
import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { InventoryTab } from "./InventoryTab";

vi.mock("next/navigation", () => import("@/test/navigation"));

const server = (overrides: Partial<ServerDetail> = {}): ServerDetail =>
  ({
    id: "s1",
    peer_id: "web-1",
    hostname: "web-1",
    site_id: "site-1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    kind: "agent",
    os_family: "debian",
    revoked_at: null,
    ...overrides,
  }) as ServerDetail;

const NOW = new Date().toISOString();

const inventory = (overrides: Partial<ServerInventory> = {}): ServerInventory => ({
  reported: { collected_at: NOW, first_at: NOW, changed_at: NOW, checked_at: NOW },
  items: [
    {
      kind: "system",
      key: "system",
      fields: { manufacturer: "Dell Inc.", model: "PowerEdge R650", serial: "7XK2LM3", bios_version: "1.10.2", bios_date: "2023-05-04", memory_bytes: String(64 * 1024 ** 3) },
    },
    { kind: "processor", key: "0", fields: { model: "Xeon Gold 6338", cores: "32", threads: "64" } },
    { kind: "processor", key: "1", fields: { model: "Xeon Gold 6338", cores: "32", threads: "64" } },
    { kind: "disk", key: "sda", fields: { model: "SAMSUNG MZ7LH480", size_bytes: "480103981056", media: "ssd" } },
    { kind: "service", key: "nginx.service", fields: { start: "enabled" } },
  ],
  package_count: 2,
  warranty: null,
  asset: { hostname: "web-1", manufacturer: "Dell Inc.", model: "PowerEdge R650", serial: "7XK2LM3" },
  ...overrides,
});

const packages = {
  reboot_required: false,
  reported_at: NOW,
  next_cursor: null,
  packages: [
    { name: "openssl", arch: "amd64", version: "3.0.14-1", upgradable_version: null, title: null, classification: null, severity: null, size_bytes: null, reboot_required: false },
    { name: "curl", arch: "amd64", version: "7.88", upgradable_version: null, title: null, classification: null, severity: null, size_bytes: null, reboot_required: false },
  ],
};

const changes = {
  changes: [
    { id: 2, at: NOW, kind: "package", key: "openssl", change: "changed", before: { version: "3.0.13" }, after: { version: "3.0.14-1" } },
    { id: 1, at: NOW, kind: "disk", key: "sdb", change: "removed", before: { model: "WDC" }, after: null },
  ],
  next_cursor: "c2",
};

describe("InventoryTab", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("shows the hardware, the tables, the packages and the changes", async () => {
    stubFetch({
      "GET /api/servers/s1/inventory": { status: 200, body: inventory() },
      "GET /api/servers/s1/packages?limit=5000": { status: 200, body: packages },
      "GET /api/servers/s1/inventory/changes?limit=50": { status: 200, body: changes },
      "GET /api/servers/s1/inventory/changes?limit=50&cursor=c2": {
        status: 200,
        body: { changes: [{ id: 0, at: NOW, kind: "software", key: "Old", change: "added", before: null, after: { version: "1" } }], next_cursor: null },
      },
    });
    render(
      <As user={viewer}>
        <InventoryTab server={server()} />
      </As>,
    );
    expect(await screen.findByText("PowerEdge R650")).toBeTruthy();
    expect(screen.getByText("7XK2LM3")).toBeTruthy();
    expect(screen.getByText("2 × Xeon Gold 6338, 64 cores, 128 threads")).toBeTruthy();
    expect(screen.getByText("64 GiB")).toBeTruthy();
    expect(screen.getByRole("table", { name: "Disks" }).textContent).toContain("SSD");
    expect(screen.getByRole("table", { name: "Services" }).textContent).toContain("At boot");
    const software = await screen.findByRole("table", { name: "Installed software" });
    expect(software.textContent).toContain("openssl");
    // The filter narrows the packages.
    const user = userEvent.setup();
    await user.type(screen.getByRole("searchbox"), "cur");
    expect(screen.getByRole("table", { name: "Installed software" }).textContent).not.toContain("openssl");
    expect(screen.getByRole("link", { name: "Export CSV" }).getAttribute("href")).toBe(
      "/api/inventory?kind=software&server_id=s1&format=csv",
    );
    const history = await screen.findByRole("table", { name: "Changes" });
    expect(history.textContent).toContain("version: 3.0.13 → 3.0.14-1");
    expect(history.textContent).toContain("Removed");
    // A viewer may not set the warranty.
    expect(screen.queryByRole("button", { name: "Set…" })).toBeNull();
    await user.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(screen.getByRole("table", { name: "Changes" }).textContent).toContain("Old"));
  });

  it("lets an operator set the warranty", async () => {
    const fetchMock = stubFetch({
      "GET /api/servers/s1/inventory": [
        { status: 200, body: inventory() },
        { status: 200, body: inventory({ warranty: { ends_on: "2099-03-31", note: "ProSupport", updated_at: NOW } }) },
      ],
      "GET /api/servers/s1/packages?limit=5000": { status: 200, body: packages },
      "GET /api/servers/s1/inventory/changes?limit=50": { status: 200, body: { changes: [], next_cursor: null } },
      "PUT /api/servers/s1/warranty": { status: 200, body: { ends_on: "2099-03-31", note: "ProSupport", updated_at: NOW } },
    });
    render(
      <As user={me({ role: "operator", customer_id: null, site_id: null })}>
        <InventoryTab server={server()} />
      </As>,
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Set…" }));
    const box = screen.getByRole("region", { name: "Warranty" });
    const [date, note] = [box.querySelector("input[type=date]")!, box.querySelector("input:not([type])")!];
    await user.type(date, "2099-03-31");
    await user.type(note, "ProSupport");
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => {
      const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
      expect(JSON.parse(String(put?.[1]?.body))).toEqual({ ends_on: "2099-03-31", note: "ProSupport" });
    });
    expect(await screen.findByText("Until 2099-03-31")).toBeTruthy();
    expect(screen.getByText("No change since the first inventory. Changes are kept for a year.")).toBeTruthy();
  });

  it("says when the agent has sent nothing, and lists Windows programs", async () => {
    stubFetch({
      "GET /api/servers/s1/inventory": { status: 200, body: inventory({ reported: null, items: [] }) },
      "GET /api/servers/s1/inventory/changes?limit=50": { status: 200, body: { changes: [], next_cursor: null } },
    });
    render(
      <As user={viewer}>
        <InventoryTab server={server()} />
      </As>,
    );
    expect(await screen.findByText("No inventory yet")).toBeTruthy();
    expect(screen.queryByRole("table", { name: "Installed software" })).toBeNull();
  });

  it("lists the programs of a Windows server from its inventory", async () => {
    stubFetch({
      "GET /api/servers/s1/inventory": {
        status: 200,
        body: inventory({
          items: [
            { kind: "software", key: "7-Zip 24.08 (x64)", fields: { version: "24.08", publisher: "Igor Pavlov" } },
            { kind: "licence", key: "Windows(R), ServerStandard edition", fields: { status: "licensed", partial_key: "3V66T" } },
          ],
        }),
      },
      "GET /api/servers/s1/inventory/changes?limit=50": { status: 200, body: { changes: [], next_cursor: null } },
    });
    render(
      <As user={viewer}>
        <InventoryTab server={server({ os_family: "windows" })} />
      </As>,
    );
    const software = await screen.findByRole("table", { name: "Installed software" });
    expect(software.textContent).toContain("Igor Pavlov");
    expect(screen.getByRole("table", { name: "Licences" }).textContent).toContain("Licensed");
  });
});
