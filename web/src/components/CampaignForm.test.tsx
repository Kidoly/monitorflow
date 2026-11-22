import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { customerAdmin } from "@/test/admin";
import { CAMPAIGN_ID, WEB1, WEB2, detail, server } from "@/test/campaigns";
import { stubFetch } from "@/test/fetch";

import { draftFromCampaign } from "@/lib/campaigns";

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
  it("speaks French when that is the language", async () => {
    setActiveLocale("fr");
    const user = userEvent.setup();
    stubFetch({
      ...inventory,
      "GET /api/servers": {
        status: 200,
        body: { servers: [server(), server({ id: WEB2, peer_id: "web-2", hostname: "web-2", package_updates: false })], next_cursor: null },
      },
    });
    render(<CampaignForm me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "Nouvelle campagne" });
    await user.selectOptions(within(form).getByLabelText("Tous les serveurs du site"), "site-1");
    expect(screen.getByRole("status")).toHaveTextContent(
      "1 serveur ciblé. 1 laissé de côté : les mises à jour de paquets n’y sont pas disponibles.",
    );
    expect(within(form).getByLabelText("Une commande non démarrée expire après")).toHaveTextContent("1 heure");
  });

  it("leaves out hosts that take no package updates, and says so", async () => {
    const user = userEvent.setup();
    stubFetch({
      ...inventory,
      "GET /api/servers": {
        status: 200,
        body: { servers: [server(), server({ id: WEB2, peer_id: "web-2", hostname: "web-2", package_updates: false })], next_cursor: null },
      },
    });
    render(<CampaignForm me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New campaign" });
    await user.selectOptions(within(form).getByLabelText("Every server of the site"), "site-1");
    expect(screen.getByText("1 server targeted. 1 left out: package updates aren't available on it.")).toBeInTheDocument();
    expect(within(form).getByRole("checkbox", { name: /web-2/ })).toBeDisabled();
    expect(within(form).getByRole("checkbox", { name: /web-2/ })).not.toBeChecked();
  });

  it("names the servers' site only when there are several", async () => {
    stubFetch({
      ...inventory,
      "GET /api/servers": {
        status: 200,
        body: { servers: [server(), server({ id: WEB2, peer_id: "web-2", hostname: "web-2", package_updates: false })], next_cursor: null },
      },
    });
    const { unmount } = render(<CampaignForm me={customerAdmin} />);
    const list = await screen.findByRole("list", { name: "Servers" });
    expect(list).toHaveTextContent(/web-1 paris · Debian 12web-2 paris · Debian 12 · .+/);
    unmount();
    render(<CampaignForm me={{ ...customerAdmin, multi_site: false }} />);
    const alone = await screen.findByRole("list", { name: "Servers" });
    expect(alone).not.toHaveTextContent("paris");
    expect(alone).toHaveTextContent(/^web-1 Debian 12web-2 Debian 12 · .+$/);
  });

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
    await user.click(screen.getByRole("button", { name: "Add batch" }));
    expect(screen.getByText(/Servers per batch: batch 1: 1, batch 2: 1/)).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "Security updates only" }));
    await user.click(screen.getByRole("button", { name: "Add window" }));
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
    await user.click(screen.getByRole("button", { name: "Add window" }));
    await user.clear(screen.getByLabelText("Time zone"));
    await user.type(screen.getByLabelText("Time zone"), "Mars/Base");
    await user.click(screen.getByRole("button", { name: "Create campaign" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("unknown time zone `Mars/Base`");
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(call?.[1]?.body as string)).toMatchObject({ server_ids: [WEB1] });
  });

  it("creates an agent update campaign when the server holds releases", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...inventory,
      "GET /api/agent-releases": { status: 200, body: { versions: ["0.3.0", "0.2.0"], latest: "0.3.0" } },
      "POST /api/campaigns": { status: 201, body: detail() },
    });
    render(<CampaignForm me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New campaign" });
    await user.type(within(form).getByLabelText("Name"), "Agents 0.3.0");
    await user.selectOptions(within(form).getByLabelText("Every server of the site"), "site-1");
    await user.click(await screen.findByRole("radio", { name: "The MonitorFlow agent" }));
    expect(screen.getByLabelText("Agent version")).toHaveValue("0.3.0");
    expect(screen.queryByRole("checkbox", { name: "Allow reboot" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Create campaign" }));
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(call?.[1]?.body as string)).toMatchObject({
      packages: [],
      security_only: false,
      allow_reboot: false,
      agent_version: "0.3.0",
    });
  });

  it("updates third-party apps alone when asked, with the managers chosen", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({ ...inventory, "POST /api/campaigns": { status: 201, body: detail() } });
    render(<CampaignForm me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New campaign" });
    await user.type(within(form).getByLabelText("Name"), "Apps");
    await user.selectOptions(within(form).getByLabelText("Every server of the site"), "site-1");
    expect(screen.getByRole("switch", { name: "Operating system" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Third-party apps" })).not.toBeChecked();
    expect(screen.queryByRole("group", { name: "App managers" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "Security updates only" }));
    await user.click(screen.getByRole("switch", { name: "Third-party apps" }));
    await user.click(screen.getByRole("switch", { name: "Operating system" }));
    // Neither: the form asks for one.
    await user.click(screen.getByRole("switch", { name: "Third-party apps" }));
    await user.click(screen.getByRole("button", { name: "Create campaign" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Choose operating system updates, third-party apps or both.");
    await user.click(screen.getByRole("switch", { name: "Third-party apps" }));
    expect(screen.queryByRole("radio", { name: "Security updates only" })).not.toBeInTheDocument();
    expect(screen.getByText(/no security classification/)).toBeInTheDocument();
    const managers = screen.getByRole("group", { name: "App managers" });
    await user.click(within(managers).getByRole("checkbox", { name: "winget" }));
    await user.click(within(managers).getByRole("checkbox", { name: "Snap" }));
    await user.click(screen.getByRole("button", { name: "Create campaign" }));
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(call?.[1]?.body as string)).toMatchObject({
      packages: [],
      security_only: false,
      include_os: false,
      third_party: [{ manager: "flatpak", packages: [] }],
    });
  });

  it("offers no agent update without releases", async () => {
    stubFetch({ ...inventory, "GET /api/agent-releases": { status: 200, body: { versions: [], latest: null } } });
    render(<CampaignForm me={customerAdmin} />);
    await screen.findByRole("form", { name: "New campaign" });
    expect(screen.queryByRole("radio", { name: "The MonitorFlow agent" })).not.toBeInTheDocument();
  });

  it("saves an edited campaign as a whole, with its batches as they were written", async () => {
    const user = userEvent.setup();
    const source = detail({
      state: "draft",
      current_wave: null,
      site_id: "site-1",
      server_ids: [],
      wave_specs: [{ percent: 50 }],
      windows: [{ kind: "weekly", weekdays: [6], start: "22:00", end: "04:00", timezone: "Europe/Paris" }],
    });
    const fetchMock = stubFetch({ ...inventory, [`PUT /api/campaigns/${CAMPAIGN_ID}`]: { status: 200, body: source } });
    render(<CampaignForm me={customerAdmin} initial={draftFromCampaign(source)} source={source} edit />);
    const form = await screen.findByRole("form", { name: "Edit October updates" });
    await user.clear(within(form).getByLabelText("Name"));
    await user.type(within(form).getByLabelText("Name"), "November updates");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(call?.[1]?.body as string)).toMatchObject({
      name: "November updates",
      site_id: "site-1",
      waves: [{ percent: 50 }],
      security_only: true,
      windows: [{ kind: "weekly", weekdays: [6], start: "22:00", end: "04:00", timezone: "Europe/Paris" }],
    });
    expect(push).toHaveBeenCalledWith(expect.stringContaining(`id=${CAMPAIGN_ID}`));
  });

  it("locks what a run under way holds and sends it as the campaign has it", async () => {
    const user = userEvent.setup();
    const source = detail({
      site_id: "site-1",
      server_ids: [],
      wave_specs: [{ size: 1 }],
      spec: { packages: ["openssl"], security_only: false, allow_reboot: true, include_os: true, third_party: [] },
    });
    const fetchMock = stubFetch({ ...inventory, [`PUT /api/campaigns/${CAMPAIGN_ID}`]: { status: 200, body: source } });
    render(<CampaignForm me={customerAdmin} initial={draftFromCampaign(source)} source={source} edit />);
    const form = await screen.findByRole("form", { name: "Edit October updates" });
    expect(screen.getByText(/A run is under way/)).toBeInTheDocument();
    expect(within(form).getAllByText("Fixed while the campaign runs.")).toHaveLength(3);
    expect(within(form).getByRole("group", { name: "Servers" })).toBeDisabled();
    expect(within(form).getByLabelText("Allow reboot")).toBeDisabled();
    await user.clear(within(form).getByLabelText("Name"));
    await user.type(within(form).getByLabelText("Name"), "Renamed");
    await user.click(screen.getByRole("button", { name: "Save changes" }));
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(call?.[1]?.body as string)).toEqual({
      customer_id: "c1",
      name: "Renamed",
      site_id: "site-1",
      server_ids: [],
      waves: [{ size: 1 }],
      packages: ["openssl"],
      security_only: false,
      allow_reboot: true,
      command_ttl_s: 3600,
      failure_threshold: { value: 0, unit: "count" },
      windows: source.windows,
    });
  });

  it("says a finished campaign is saved for its next run", async () => {
    const idle = { ...detail().progress, in_progress: 0 };
    const source = detail({ state: "failed", finished_at: "2026-10-23T10:00:00Z", progress: idle });
    stubFetch(inventory);
    render(<CampaignForm me={customerAdmin} initial={draftFromCampaign(source)} source={source} edit />);
    expect(await screen.findByText(/is over\. Saving keeps its last run/)).toBeInTheDocument();
    expect(screen.queryByText("Fixed while the campaign runs.")).not.toBeInTheDocument();
  });

  it("searches servers, pages through them, chooses all results and removes one", async () => {
    const user = userEvent.setup();
    const many = Array.from({ length: 120 }, (_, i) => {
      const n = String(i).padStart(3, "0");
      const windows = i % 2 === 1;
      return server({
        id: `s-${n}`,
        peer_id: `host-${n}`,
        hostname: `host-${n}`,
        ...(windows && { os_family: "windows", os_version: "Windows Server 2022" }),
      });
    });
    stubFetch({ ...inventory, "GET /api/servers": { status: 200, body: { servers: many, next_cursor: null } } });
    render(<CampaignForm me={customerAdmin} />);
    const list = await screen.findByRole("list", { name: "Servers" });
    expect(within(list).getAllByRole("checkbox")).toHaveLength(50);
    expect(screen.getByText("1–50 of 120")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Next" }));
    expect(screen.getByText("51–100 of 120")).toBeInTheDocument();
    expect(within(list).getByText("host-050")).toBeInTheDocument();

    await user.type(screen.getByLabelText("Search servers"), "windows 2022");
    expect(screen.getByText("1–50 of 60")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Choose the 60 results" }));
    expect(screen.getByRole("status")).toHaveTextContent("60 servers targeted.");
    const chosen = screen.getByRole("group", { name: "60 servers chosen" });
    await user.click(within(chosen).getByRole("button", { name: "Remove host-001" }));
    expect(screen.getByRole("status")).toHaveTextContent("59 servers targeted.");
    expect(screen.getByRole("button", { name: "Choose the 1 result" })).toBeEnabled();

    await user.clear(screen.getByLabelText("Search servers"));
    await user.type(screen.getByLabelText("Search servers"), "nothing-like-this");
    expect(screen.getByText("No server matches.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove all" }));
    expect(screen.getByRole("status")).toHaveTextContent("0 servers targeted.");
  });

  it("makes a window a card that repeats every week, month or year", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({ ...inventory, "POST /api/campaigns": { status: 201, body: detail() } });
    render(<CampaignForm me={customerAdmin} />);
    await user.type(await screen.findByLabelText("Name"), "Patch Tuesday");
    await user.selectOptions(screen.getByLabelText("Every server of the site"), "site-1");
    await user.click(screen.getByRole("button", { name: "Add window" }));
    await user.click(screen.getByRole("button", { name: "Add window" }));
    const first = screen.getByRole("group", { name: "Window 1" });
    const days = within(first).getByRole("group", { name: "Days" });
    expect(within(days).getByRole("button", { name: "Sat" })).toHaveAttribute("aria-pressed", "true");
    await user.click(within(days).getByRole("button", { name: "Sun" }));
    await user.click(within(days).getByRole("button", { name: "Fri" }));
    expect(within(first).getByText(/^Fri, Sat 22:00–04:00/)).toBeInTheDocument();

    const second = screen.getByRole("group", { name: "Window 2" });
    await user.selectOptions(within(second).getByLabelText("Repeats"), "monthly");
    await user.selectOptions(within(second).getByLabelText("On"), "weekday");
    await user.selectOptions(within(second).getByLabelText("Week"), "2");
    await user.selectOptions(within(second).getByLabelText("Weekday"), "2");
    expect(within(second).getByText(/^Second Tuesday of each month 22:00–04:00/)).toBeInTheDocument();
    await user.click(within(within(second).getByRole("group", { name: "Months" })).getByRole("button", { name: "Feb" }));
    expect(within(second).getByText(/^Second Tuesday of Jan, Mar,/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Add window" }));
    const third = screen.getByRole("group", { name: "Window 3" });
    await user.selectOptions(within(third).getByLabelText("Repeats"), "yearly");
    await user.selectOptions(within(third).getByLabelText("Month"), "12");
    await user.clear(within(third).getByLabelText("Day"));
    await user.type(within(third).getByLabelText("Day"), "31");
    expect(within(third).getByText("A month without day 31 has no window.")).toBeInTheDocument();
    expect(within(third).getByText(/^Every year on December 31 22:00–04:00/)).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Create campaign" }));
    const call = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    const zone = expect.any(String);
    expect(JSON.parse(call?.[1]?.body as string).windows).toEqual([
      { kind: "weekly", weekdays: [5, 6], start: "22:00", end: "04:00", timezone: zone },
      {
        kind: "monthly",
        month_week: 2,
        weekday: 2,
        months: [1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        start: "22:00",
        end: "04:00",
        timezone: zone,
      },
      { kind: "monthly", month_day: 31, months: [12], start: "22:00", end: "04:00", timezone: zone },
    ]);
  });
});
