import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { CSRF_HEADER } from "@/lib/api";
import { customerAdmin, me } from "@/test/admin";
import { CAMPAIGN_ID, WEB1, detail } from "@/test/campaigns";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { CampaignPage } from "./CampaignPage";
import { REFRESH_MS } from "./FleetOverview";

const PATH = `/api/campaigns/${CAMPAIGN_ID}`;

describe("CampaignPage", () => {
  it("shows progress per wave and per server", async () => {
    const waves = detail().waves;
    waves[0]!.servers[0] = { ...waves[0]!.servers[0]!, status: "failed", exit_code: 100, error: "dpkg lock" };
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: detail({ waves }) } });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByRole("heading", { level: 1, name: "October updates" })).toBeInTheDocument();
    expect(screen.getByText("Security updates")).toBeInTheDocument();
    expect(screen.getByText("Sat, Sun 22:00–04:00 (Europe/Paris)")).toBeInTheDocument();
    const first = screen.getByRole("region", { name: "Wave 1" });
    const row = within(first).getByRole("link", { name: "web-1" }).closest("tr")!;
    expect(within(row).getByRole("link")).toHaveAttribute("href", expect.stringContaining(`id=${WEB1}`));
    expect(row).toHaveTextContent("failed");
    expect(row).toHaveTextContent("100");
    expect(row).toHaveTextContent("dpkg lock");
    expect(within(screen.getByRole("region", { name: "Wave 2" })).getByText("pending", { selector: "td span" })).toBeInTheDocument();
  });

  it("enables the actions the state allows and applies their answer", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: { status: 200, body: detail() },
      [`POST ${PATH}/pause`]: { status: 200, body: detail({ state: "paused" }) },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    const pause = await screen.findByRole("button", { name: "Pause" });
    expect(screen.getByRole("button", { name: "Start" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Resume" })).toBeDisabled();
    await userEvent.click(pause);
    expect(await screen.findByRole("button", { name: "Resume" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Pause" })).toBeDisabled();
    const call = fetchMock.mock.calls.findIndex(([url]) => String(url).endsWith("/pause"));
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
  });

  it("stops only after confirmation and shows API errors", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: { status: 200, body: detail() },
      [`POST ${PATH}/stop`]: { status: 409, body: { error: "cannot stop a campaign that is completed" } },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    await userEvent.click(await screen.findByRole("button", { name: "Stop…" }));
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(fetchMock).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole("button", { name: "Stop…" }));
    await userEvent.click(screen.getByRole("button", { name: "Stop the campaign" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("cannot stop a campaign that is completed");
  });

  it("has no actions for readers", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: detail() } });
    render(<CampaignPage id={CAMPAIGN_ID} me={me({ role: "viewer", customer_id: "c1", site_id: null })} />);
    await screen.findByRole("heading", { level: 1 });
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("refreshes while running and stops once settled", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const fetchMock = stubFetch({
        [`GET ${PATH}`]: [
          { status: 200, body: detail() },
          {
            status: 200,
            body: detail({ state: "completed", progress: { ...detail().progress, in_progress: 0 } }),
          },
        ],
      });
      render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
      expect(await screen.findByText("running", { selector: "dd span" })).toBeInTheDocument();
      await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
      expect(await screen.findByText("completed", { selector: "dd span" })).toBeInTheDocument();
      await act(() => vi.advanceTimersByTimeAsync(3 * REFRESH_MS));
      expect(fetchMock).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("says when the campaign is not found", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 404, body: { error: "not found" } } });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByRole("heading", { name: "Campaign not found" })).toBeInTheDocument();
  });
});
