import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { NO_FILTERS } from "@/lib/incidents";
import { stubFetch } from "@/test/fetch";
import { SERVER_ID, SITE_ID, incident } from "@/test/incidents";
import { setUrl } from "@/test/navigation";

import { REFRESH_MS } from "./FleetOverview";
import { IncidentList, IncidentsView, PAGE_SIZE } from "./IncidentList";

vi.mock("next/navigation", () => import("@/test/navigation"));

const NOW = new Date(2026, 9, 19, 14, 0);
const ACME = "0b8f3c4e-1a2b-4c3d-8e9f-000000000001";
const OTHER_SITE = "5a1e0c2d-0000-4000-8000-000000000003";

const options = {
  "GET /api/sites": {
    status: 200,
    body: {
      sites: [
        { id: SITE_ID, name: "paris", server_count: 1, relay_count: 0, customer_id: ACME, customer_name: "Acme Hosting" },
        { id: OTHER_SITE, name: "berlin", server_count: 1, relay_count: 0, customer_id: "c2", customer_name: "Northwind" },
      ],
    },
  },
  "GET /api/servers": {
    status: 200,
    body: { servers: [{ id: SERVER_ID, hostname: "web-1", site_id: SITE_ID, customer_id: ACME }], next_cursor: null },
  },
};

const open = incident({ opened_at: new Date(2026, 9, 19, 9, 40).toISOString() });
const resolved = incident({
  id: "2d6a9a3e-5b7c-4d8e-9f01-23456789abcd",
  hostname: "db-1",
  server_id: "7f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02",
  site_id: OTHER_SITE,
  site_name: "berlin",
  metric: "monitorflow_memory_used_bytes",
  instance: "",
  severity: "warning",
  state: "resolved",
  opened_at: new Date(2026, 9, 18, 23, 10).toISOString(),
  resolved_at: new Date(2026, 9, 19, 1, 42).toISOString(),
  last_value: 1_610_612_736,
  peak_value: 2_147_483_648,
});

const OPEN = "GET /api/incidents?state=open&limit=500";
const openReply = { status: 200, body: { incidents: [open], next_cursor: null } };
const table = () => screen.findByRole("table", { name: "Incidents" });

describe("IncidentList", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl("/incidents/");
  });
  afterEach(() => vi.useRealTimers());

  it("lists incidents with readable metrics, values and durations", async () => {
    stubFetch({
      ...options,
      [OPEN]: openReply,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open, resolved], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(/^Critical\s*web-1paris · Acme Hosting\s*Disk used \/var\s*09:40 today\s*Open\s*4 h 20 min\s*96\.2%\s*97\.1%$/);
    expect(rows[2]).toHaveTextContent(/^Resolved was Warning\s*db-1berlin · Northwind\s*Memory used\s*Yesterday 23:10\s*Resolved 01:42\s*2 h 32 min\s*1\.5 GiB\s*2 GiB$/);
    expect(within(rows[1]!).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/view/?\\?id=${SERVER_ID}$`)),
    );
    expect(within(rows[1]!).getByRole("link", { name: "Disk used /var" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/incidents/view/?\\?id=${open.id}$`)),
    );
    expect(screen.getByText(/1 open, 1 critical · Updated 14:00:00/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
    expect(screen.getByText("Showing 2 incidents · 50 per page")).toBeInTheDocument();
  });

  it("loads 50 more and keeps every loaded incident on refresh", async () => {
    const page = (n: number) => Array.from({ length: n }, (_, i) => incident({ id: `00000000-0000-4000-8000-${String(i).padStart(12, "0")}` }));
    const fetchMock = stubFetch({
      ...options,
      [OPEN]: openReply,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: page(PAGE_SIZE), next_cursor: "c1" } },
      [`GET /api/incidents?limit=${2 * PAGE_SIZE}`]: { status: 200, body: { incidents: page(PAGE_SIZE + 10), next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    await userEvent.click(await screen.findByRole("button", { name: "Load more" }));
    await vi.waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(PAGE_SIZE + 11));
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(fetchMock.mock.calls.filter(([url]) => url === `/api/incidents?limit=${2 * PAGE_SIZE}`).length).toBeGreaterThan(1);
    expect(screen.getAllByRole("row")).toHaveLength(PAGE_SIZE + 11);
  });

  it("passes filters to the API and writes changes to the URL", async () => {
    setUrl(`/incidents/?state=open&server=${SERVER_ID}`);
    stubFetch({
      ...options,
      [OPEN]: openReply,
      [`GET /api/incidents?state=open&server_id=${SERVER_ID}&limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [], next_cursor: null } },
    });
    const filters = { ...NO_FILTERS, state: "open", serverId: SERVER_ID } as const;
    render(<IncidentList filters={filters} />);
    expect(await screen.findByText("No incidents match these filters.")).toBeInTheDocument();
    expect(await screen.findByRole("option", { name: "Server: web-1" })).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Server" })).toHaveValue(SERVER_ID);

    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Severity" }), "critical");
    expect(window.location.search).toBe(`?state=open&server=${SERVER_ID}&severity=critical`);
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Site" }), "Site: berlin");
    expect(window.location.search).toBe(`?state=open&severity=critical&site=${OTHER_SITE}`);
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(window.location.search).toBe("");
  });

  it("keeps the scope's customer only", async () => {
    setUrl(`/incidents/?customer=${ACME}`);
    stubFetch({
      ...options,
      [OPEN]: { status: 200, body: { incidents: [open, { ...resolved, state: "open" }], next_cursor: null } },
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open, resolved], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    await vi.waitFor(async () => expect(within(await table()).getAllByRole("row")).toHaveLength(2));
    expect(screen.getByText(/1 open, 1 critical/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "web-1" })).toHaveAttribute("href", expect.stringContaining(`customer=${ACME}`));
  });

  it("says when there are no incidents at all", async () => {
    stubFetch({ ...options, [OPEN]: openReply, [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [], next_cursor: null } } });
    render(<IncidentList filters={NO_FILTERS} />);
    expect(await screen.findByText(/No incidents\. No threshold has fired/)).toBeInTheDocument();
  });

  it("shows errors and keeps the last list when a refresh fails", async () => {
    stubFetch({
      ...options,
      [OPEN]: openReply,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: [
        { status: 500, body: { error: "internal error" } },
        { status: 200, body: { incidents: [open], next_cursor: null } },
        { status: 502, body: { error: "bad gateway" } },
      ],
    });
    render(<IncidentList filters={NO_FILTERS} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await table()).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect((await screen.findByText(/Refresh failed/)).closest("[role=status]")).toHaveTextContent(/Showing the last data received/);
    expect(screen.getByRole("table")).toBeInTheDocument();
  });
});

describe("IncidentsView", () => {
  it("reads filters from the query string", async () => {
    setUrl("/incidents/?state=resolved&severity=warning&bogus=1");
    stubFetch({
      ...options,
      [OPEN]: openReply,
      [`GET /api/incidents?state=resolved&severity=warning&limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [resolved], next_cursor: null } },
    });
    render(<IncidentsView />);
    expect(await table()).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "State" })).toHaveValue("resolved");
  });
});
