import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { PsaAssetLink, PsaAssetSettings } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { PsaAssets } from "./PsaAssets";

vi.mock("next/navigation", () => import("@/test/navigation"));

const off: PsaAssetSettings = { asset_sync: false, asset_inactive_on_removal: false, types: [] };
const on: PsaAssetSettings = {
  asset_sync: true,
  asset_inactive_on_removal: false,
  types: [
    {
      family: "linux",
      type_id: 3,
      type_name: "Server",
      hostname_field: "inventory_number",
      serial_field: "12",
      manufacturer_field: null,
      model_field: null,
    },
  ],
};

function link(overrides: Partial<PsaAssetLink> = {}): PsaAssetLink {
  return {
    server_id: "s1",
    hostname: "web-01",
    site_name: "paris",
    state: "proposed",
    asset_id: 900,
    asset_name: "OLD-NAME",
    matched_by: "serial",
    deactivated: false,
    error: null,
    pushed_at: null,
    updated_at: "2026-11-25T10:00:00Z",
    ...overrides,
  };
}

const lists = {
  "GET /api/psa/asset-types": { status: 200, body: { records: [{ id: 3, name: "Server" }, { id: 4, name: "Workstation" }] } },
  "GET /api/psa/asset-types/3/fields": {
    status: 200,
    body: { records: [{ id: 13, name: "Manufacturer" }, { id: 12, name: "Serial Number" }] },
  },
};

describe("PsaAssets", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setActiveLocale("en");
  });

  it("chooses an asset type and fields per family and turns the sync on", async () => {
    const fetchMock = stubFetch({
      ...lists,
      "GET /api/psa/asset-settings": [{ status: 200, body: off }, { status: 200, body: on }],
      "PUT /api/psa/asset-settings": { status: 200, body: on },
      "GET /api/psa/asset-links": { status: 200, body: { servers: [], proposed: 0, linked: 0, failed: 0 } },
    });
    const user = userEvent.setup();
    render(<PsaAssets />);
    const linux = await screen.findByRole("group", { name: "Linux servers" });
    await user.selectOptions(within(linux).getByLabelText("Asset type"), "3");
    const serial = within(linux).getByLabelText("Serial number goes to");
    await vi.waitFor(() => expect(serial).toBeEnabled());
    await user.selectOptions(serial, "12");
    await user.selectOptions(within(linux).getByLabelText("Host name goes to"), "inventory_number");
    await user.click(screen.getByRole("checkbox", { name: "Sync servers as HaloPSA assets" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    const put = fetchMock.mock.calls.find(([u, init]) => u === "/api/psa/asset-settings" && init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({
      asset_sync: true,
      asset_inactive_on_removal: false,
      types: [
        {
          family: "linux",
          type_id: 3,
          hostname_field: "inventory_number",
          serial_field: "12",
          manufacturer_field: null,
          model_field: null,
        },
      ],
    });
    expect(await screen.findByText("No server synced yet. They show here within minutes of turning the sync on.")).toBeInTheDocument();
  });

  it("confirms a proposed asset or asks for a new one", async () => {
    const fetchMock = stubFetch({
      ...lists,
      "GET /api/psa/asset-settings": { status: 200, body: on },
      "GET /api/psa/asset-links": { status: 200, body: { servers: [link()], proposed: 1, linked: 4, failed: 0 } },
      "PUT /api/psa/asset-links/s1": { status: 200, body: link({ state: "linked" }) },
      "POST /api/psa/asset-links/sync": { status: 200, body: { queued: 4 } },
    });
    const user = userEvent.setup();
    render(<PsaAssets />);
    const table = await screen.findByRole("table", { name: "Servers and their HaloPSA assets" });
    expect(within(table).getByText("To confirm")).toBeInTheDocument();
    expect(within(table).getByText("Same serial number")).toBeInTheDocument();
    expect(within(table).getByText("OLD-NAME (#900)")).toBeInTheDocument();
    expect(screen.getByText("Linked: 4. To confirm: 1. Refused: 0.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Confirm the asset of web-01" }));
    await user.click(screen.getByRole("button", { name: "Create a new asset for web-01" }));
    const bodies = fetchMock.mock.calls
      .filter(([u, init]) => u === "/api/psa/asset-links/s1" && init?.method === "PUT")
      .map(([, init]) => JSON.parse(String(init?.body)));
    expect(bodies).toEqual([{ asset_id: 900 }, { asset_id: null }]);
    await user.click(screen.getByRole("button", { name: "Sync now" }));
    expect(await screen.findByText("4 to send again")).toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ ...lists, "GET /api/psa/asset-settings": { status: 200, body: off } });
    render(<PsaAssets />);
    expect(await screen.findByRole("group", { name: "Serveurs Linux" })).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Synchroniser les serveurs comme assets HaloPSA" })).toBeInTheDocument();
  });
});
