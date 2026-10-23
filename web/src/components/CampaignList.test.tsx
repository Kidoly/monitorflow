import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, customerAdmin, me, siteAdmin } from "@/test/admin";
import { CAMPAIGN_ID, WEB1, campaign, detail } from "@/test/campaigns";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { CampaignListPage, waveText } from "./CampaignList";

vi.mock("next/navigation", () => import("@/test/navigation"));

const LIST = "GET /api/campaigns?limit=500";
const SITES = { "GET /api/sites": { status: 200, body: { sites: [{ id: "site-1", name: "paris-dc1" }] } } };

function routes(campaigns = [campaign(), campaign({ id: "c-2", name: "Draft", state: "draft", current_wave: null })], more = false) {
  return {
    ...SITES,
    [LIST]: { status: 200, body: { campaigns, next_cursor: more ? "x" : null } },
    [`GET /api/campaigns/${CAMPAIGN_ID}`]: { status: 200, body: detail() },
  };
}

describe("CampaignListPage", () => {
  beforeEach(() => setUrl("/campaigns/"));

  it("lists campaigns with their state, wave and counts", async () => {
    stubFetch(routes(undefined, true));
    render(
      <As user={customerAdmin}>
        <CampaignListPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Campaigns" });
    const row = within(table).getByRole("link", { name: "October updates" }).closest("tr")!;
    expect(within(row).getByRole("link")).toHaveAttribute("href", expect.stringContaining(`id=${CAMPAIGN_ID}`));
    expect(row).toHaveTextContent("2 waves · security only");
    expect(row).toHaveTextContent("Running wave 1 of 2");
    expect(row).toHaveTextContent("0 / 2 · 1 in progress");
    expect(row).toHaveTextContent("Sat, Sun 22:00–04:00");
    expect(within(table).getByRole("link", { name: "Draft" }).closest("tr")).toHaveTextContent("Draft");
    expect(screen.getByRole("link", { name: "New campaign" })).toBeInTheDocument();
    expect(screen.getByText(/Showing the newest 2 campaigns\./)).toBeInTheDocument();

    const tiles = screen.getByRole("group", { name: "Summary, click to filter" });
    expect(within(tiles).getByRole("link", { name: /Running/ })).toHaveTextContent("Running1");
    expect(within(tiles).getByRole("link", { name: /Succeeded, 30 d/ })).toHaveTextContent("0");
  });

  it("shows the servers of the running campaign", async () => {
    stubFetch(routes());
    render(
      <As user={customerAdmin}>
        <CampaignListPage />
      </As>,
    );
    const servers = await screen.findByRole("table", { name: "Servers in the running campaign" });
    expect(within(servers).getByText("Server · October updates")).toBeInTheDocument();
    const first = within(servers).getAllByRole("row")[1]!;
    expect(within(first).getByRole("link", { name: /web-1/ })).toHaveAttribute("href", expect.stringContaining(`id=${WEB1}`));
    expect(first).toHaveTextContent("paris-dc1");
    expect(first).toHaveTextContent("1 of 2");
    expect(first).toHaveTextContent("Running");
  });

  it("filters by state in the URL and acts from the row", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...routes(),
      [`POST /api/campaigns/${CAMPAIGN_ID}/pause`]: { status: 200, body: detail({ state: "paused" }) },
    });
    render(
      <As user={customerAdmin}>
        <CampaignListPage />
      </As>,
    );
    await user.selectOptions(await screen.findByRole("combobox", { name: "State" }), "draft");
    expect(window.location.search).toBe("?state=draft");
    const table = screen.getByRole("table", { name: "Campaigns" });
    expect(within(table).queryByRole("link", { name: "October updates" })).not.toBeInTheDocument();
    expect(within(table).getByRole("button", { name: "Start Draft" })).toBeInTheDocument();

    await user.selectOptions(screen.getByRole("combobox", { name: "State" }), "");
    await user.click(screen.getByRole("button", { name: "Pause October updates" }));
    expect(fetchMock).toHaveBeenCalledWith(`/api/campaigns/${CAMPAIGN_ID}/pause`, expect.objectContaining({ method: "POST" }));
  });

  it("keeps to the customer of the scope", async () => {
    setUrl("/campaigns/?customer=0b8f3c4e-1a2b-4c3d-8e9f-000000000002");
    stubFetch(routes());
    render(
      <As user={customerAdmin}>
        <CampaignListPage />
      </As>,
    );
    expect(await screen.findByText("No campaigns yet")).toBeInTheDocument();
  });

  it("hides creation from readers and refuses site-only roles", async () => {
    stubFetch(routes([]));
    const { unmount } = render(
      <As user={me({ role: "viewer", customer_id: "c1", site_id: null })}>
        <CampaignListPage />
      </As>,
    );
    expect(await screen.findByText("No campaigns yet")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "New campaign" })).not.toBeInTheDocument();
    unmount();

    render(
      <As user={siteAdmin}>
        <CampaignListPage />
      </As>,
    );
    expect(screen.getByText(/need a role on a whole customer/)).toBeInTheDocument();
  });

  it("names the current wave once queued", () => {
    expect(waveText({ current_wave: null, wave_count: 1 })).toBe("1 wave");
    expect(waveText({ current_wave: 2, wave_count: 3 })).toBe("Wave 3 of 3");
  });
});
