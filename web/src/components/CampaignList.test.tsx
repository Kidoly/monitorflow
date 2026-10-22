import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { As, customerAdmin, me, siteAdmin } from "@/test/admin";
import { CAMPAIGN_ID, campaign } from "@/test/campaigns";
import { stubFetch } from "@/test/fetch";

import { CampaignListPage, waveText } from "./CampaignList";

describe("CampaignListPage", () => {
  it("lists campaigns with their state, wave and counts", async () => {
    stubFetch({
      "GET /api/campaigns": {
        status: 200,
        body: { campaigns: [campaign(), campaign({ id: "c-2", name: "Draft", state: "draft", current_wave: null })], next_cursor: "x" },
      },
    });
    render(
      <As user={customerAdmin}>
        <CampaignListPage />
      </As>,
    );
    const row = (await screen.findByRole("link", { name: "October updates" })).closest("tr")!;
    expect(within(row).getByRole("link")).toHaveAttribute("href", expect.stringContaining(`id=${CAMPAIGN_ID}`));
    expect(row).toHaveTextContent("running");
    expect(row).toHaveTextContent("Wave 1 of 2");
    expect(row).toHaveTextContent("0 succeeded, 1 in progress, 1 pending of 2");
    expect(screen.getByRole("link", { name: "Draft" }).closest("tr")).toHaveTextContent("2 waves");
    expect(screen.getByRole("link", { name: "New campaign" })).toBeInTheDocument();
    expect(screen.getByText("Showing the newest 2 campaigns.")).toBeInTheDocument();
  });

  it("hides creation from readers and refuses site-only roles", async () => {
    stubFetch({ "GET /api/campaigns": { status: 200, body: { campaigns: [], next_cursor: null } } });
    const { unmount } = render(
      <As user={me({ role: "viewer", customer_id: "c1", site_id: null })}>
        <CampaignListPage />
      </As>,
    );
    expect(await screen.findByText("No campaigns yet.")).toBeInTheDocument();
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
