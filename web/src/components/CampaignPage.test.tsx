import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { CSRF_HEADER } from "@/lib/api";
import { customerAdmin, me, single } from "@/test/admin";
import { CAMPAIGN_ID, WEB1, detail } from "@/test/campaigns";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { CampaignPage } from "./CampaignPage";
import { REFRESH_MS } from "./FleetOverview";

const PATH = `/api/campaigns/${CAMPAIGN_ID}`;

const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ push }), useSearchParams: () => new URLSearchParams() }));

describe("CampaignPage", () => {
  it("shows progress per batch and per server", async () => {
    const waves = detail().waves;
    waves[0]!.servers[0] = { ...waves[0]!.servers[0]!, status: "failed", exit_code: 100, error: "dpkg lock" };
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: detail({ waves }) } });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByRole("heading", { level: 1, name: "October updates" })).toBeInTheDocument();
    expect(screen.getByText("Security updates")).toBeInTheDocument();
    expect(screen.getByText("Sat, Sun 22:00–04:00 (Europe/Paris)")).toBeInTheDocument();
    const first = screen.getByRole("region", { name: "Batch 1" });
    const row = within(first).getByRole("link", { name: "web-1" }).closest("tr")!;
    expect(within(row).getByRole("link")).toHaveAttribute("href", expect.stringContaining(`id=${WEB1}`));
    expect(row).toHaveTextContent("Failed");
    expect(row).toHaveTextContent("100");
    expect(row).toHaveTextContent("dpkg lock");
    expect(within(screen.getByRole("region", { name: "Batch 2" })).getByText("Pending", { selector: "td span" })).toBeInTheDocument();
    expect(within(first).getByRole("progressbar", { name: "Batch 1 progress" })).toHaveAttribute("value", "0");
    expect(screen.getByRole("group", { name: "Servers by status" })).toHaveTextContent("In progress1");
    expect(screen.getByText("Customer")).toBeInTheDocument();
    // Numbers are the counters'; a batch line has its bar, and a server row no "Not yet".
    expect(first).not.toHaveTextContent(/succeeded of|not started/);
    expect(first).not.toHaveTextContent("Not yet");
  });

  it("says a scheduled campaign's start was only requested", async () => {
    stubFetch({
      [`GET ${PATH}`]: {
        status: 200,
        body: detail({ state: "scheduled", current_wave: null, started_at: "2026-11-02T09:30:00Z" }),
      },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByText("Start requested")).toBeInTheDocument();
    expect(screen.queryByText("Started")).not.toBeInTheDocument();
  });

  it("names no customer for a single company or a customer's user (ADR-0104)", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: detail() } });
    render(<CampaignPage id={CAMPAIGN_ID} me={single(customerAdmin)} />);
    expect(await screen.findByRole("heading", { level: 1, name: "October updates" })).toBeInTheDocument();
    expect(screen.queryByText("Customer")).not.toBeInTheDocument();
  });

  it("offers the actions the state allows and applies their answer", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: { status: 200, body: detail() },
      [`POST ${PATH}/pause`]: { status: 200, body: detail({ state: "paused" }) },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    const pause = await screen.findByRole("button", { name: "Pause" });
    expect(screen.queryByRole("button", { name: "Start" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Resume" })).not.toBeInTheDocument();
    await userEvent.click(pause);
    expect(await screen.findByRole("button", { name: "Resume" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Pause" })).not.toBeInTheDocument();
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
      expect(await screen.findByText("Running", { selector: "h1 + span" })).toBeInTheDocument();
      await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
      expect(await screen.findByText("Succeeded", { selector: "h1 + span" })).toBeInTheDocument();
      await act(() => vi.advanceTimersByTimeAsync(3 * REFRESH_MS));
      expect(fetchMock).toHaveBeenCalledTimes(2);
    } finally {
      vi.useRealTimers();
    }
  });

  it("says when the campaign is not found", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 404, body: { error: "not found" } } });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByText("Campaign not found")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Back to campaigns" })).toBeInTheDocument();
  });

  it("shows the pinned snapshots", async () => {
    stubFetch({
      [`GET ${PATH}`]: {
        status: 200,
        body: detail({ sources: { mode: "replace", mirrors: [{ name: "debian-12", snapshot: "s1" }] } }),
      },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByText("debian-12/s1 (other sources set aside)")).toBeInTheDocument();
  });

  it("edits and copies a campaign that queued nothing, and deletes a draft after confirmation", async () => {
    const fetchMock = stubFetch({
      [`GET ${PATH}`]: { status: 200, body: detail({ state: "draft", current_wave: null, started_at: null }) },
      [`DELETE ${PATH}`]: { status: 204 },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    const edit = await screen.findByRole("link", { name: "Edit" });
    expect(edit).toHaveAttribute("href", expect.stringContaining(`new?edit=${CAMPAIGN_ID}`));
    const copy = screen.getByRole("link", { name: "Duplicate" });
    expect(copy).toHaveAttribute("href", expect.stringContaining(`new?from=${CAMPAIGN_ID}`));
    // A draft is deleted, not stopped.
    expect(screen.queryByRole("button", { name: "Stop…" })).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Delete…" }));
    expect(screen.getByText(/the audit log records who deleted it/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Delete the campaign" }));
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(true);
    expect(push).toHaveBeenCalledWith("/campaigns/");
  });

  it("only copies or stops a running campaign, and only copies or deletes a finished one", async () => {
    stubFetch({ [`GET ${PATH}`]: { status: 200, body: detail() } });
    const { unmount } = render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByRole("link", { name: "Duplicate" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop…" })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Delete…" })).not.toBeInTheDocument();
    unmount();
    const idle = { ...detail().progress, in_progress: 0 };
    stubFetch({
      [`GET ${PATH}`]: { status: 200, body: detail({ state: "failed", finished_at: "2026-10-23T10:00:00Z", progress: idle }) },
    });
    render(<CampaignPage id={CAMPAIGN_ID} me={customerAdmin} />);
    expect(await screen.findByRole("button", { name: "Delete…" })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Stop…" })).not.toBeInTheDocument();
  });
});
