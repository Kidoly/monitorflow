import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { customerAdmin } from "@/test/admin";
import { CAMPAIGN_ID, WEB1, WEB2, detail, server } from "@/test/campaigns";
import { stubFetch } from "@/test/fetch";

import { CampaignForm } from "./CampaignForm";

const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ push }) }));

const inventory = {
  "GET /api/customers": {
    status: 200,
    body: {
      customers: [
        { id: "c1", name: "acme", site_count: 1 },
        { id: "c9", name: "other", site_count: 1 },
      ],
    },
  },
  "GET /api/sites": {
    status: 200,
    body: {
      sites: [{ id: "site-1", name: "paris", server_count: 2, relay_count: 0, customer_id: "c1", customer_name: "acme" }],
    },
  },
  "GET /api/servers": {
    status: 200,
    body: { servers: [server(), server({ id: WEB2, peer_id: "web-2", hostname: "web-2" })], next_cursor: null },
  },
};

describe("CampaignForm", () => {
  it("creates a campaign with a pilot wave and a window, then opens it", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({ ...inventory, "POST /api/campaigns": { status: 201, body: detail() } });
    render(<CampaignForm me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New campaign" });
    // Only administered customers are offered.
    expect(within(form).getByLabelText("Customer")).toHaveTextContent("acme");
    expect(within(form).getByLabelText("Customer")).not.toHaveTextContent("other");

    await user.type(within(form).getByLabelText("Name"), "October updates");
    await user.selectOptions(within(form).getByLabelText("Every server of the site"), "site-1");
    expect(screen.getByText("2 servers targeted.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add wave" }));
    expect(screen.getByText(/Servers per wave: wave 1: 1, wave 2: 1/)).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "Security updates only" }));
    await user.click(screen.getByRole("button", { name: "Add weekly window" }));
    const window = screen.getByRole("group", { name: "Window 1" });
    await user.clear(within(window).getByLabelText("Time zone"));
    await user.type(within(window).getByLabelText("Time zone"), "Europe/Paris");
    await user.clear(screen.getByLabelText("Stop when more than"));
    await user.type(screen.getByLabelText("Stop when more than"), "1");
    await user.click(screen.getByRole("button", { name: "Create campaign" }));

    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(call?.[1]?.body as string)).toEqual({
      customer_id: "c1",
      name: "October updates",
      site_id: "site-1",
      server_ids: [],
      waves: [{ size: 1 }],
      packages: [],
      security_only: true,
      allow_reboot: false,
      command_ttl_s: 3600,
      failure_threshold: { value: 1, unit: "count" },
      windows: [{ kind: "weekly", weekdays: [6, 7], start: "22:00", end: "04:00", timezone: "Europe/Paris" }],
    });
    expect(push).toHaveBeenCalledWith(expect.stringContaining(`id=${CAMPAIGN_ID}`));
  });

  it("shows client-side errors without sending, then API errors", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...inventory,
      "POST /api/campaigns": { status: 400, body: { error: "unknown time zone `Mars/Base`" } },
    });
    render(<CampaignForm me={customerAdmin} />);
    await user.click(await screen.findByRole("button", { name: "Create campaign" }));
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("The name must be 1 to 64 characters");
    expect(alert).toHaveTextContent("Choose a site or servers to update.");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);

    await user.type(screen.getByLabelText("Name"), "October");
    await user.click(screen.getByRole("checkbox", { name: /web-1/ }));
    await user.click(screen.getByRole("button", { name: "Add weekly window" }));
    await user.clear(screen.getByLabelText("Time zone"));
    await user.type(screen.getByLabelText("Time zone"), "Mars/Base");
    await user.click(screen.getByRole("button", { name: "Create campaign" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("unknown time zone `Mars/Base`");
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(call?.[1]?.body as string)).toMatchObject({ server_ids: [WEB1] });
  });
});
