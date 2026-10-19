import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { NO_FILTERS } from "@/lib/incidents";
import { stubFetch } from "@/test/fetch";
import { SERVER_ID, SITE_ID, incident } from "@/test/incidents";

import { REFRESH_MS } from "./FleetOverview";
import { IncidentList, IncidentsView, PAGE_SIZE } from "./IncidentList";

const replace = vi.fn();
let search = "";
vi.mock("next/navigation", () => ({
  useRouter: () => ({ replace }),
  useSearchParams: () => new URLSearchParams(search),
}));

const NOW = new Date("2026-10-19T12:00:00Z");

const options = {
  "GET /api/sites": { status: 200, body: { sites: [{ id: SITE_ID, name: "paris", server_count: 1 }] } },
  "GET /api/servers": {
    status: 200,
    body: { servers: [{ id: SERVER_ID, hostname: "web-1", site_id: SITE_ID }], next_cursor: null },
  },
};

const open = incident();
const resolved = incident({
  id: "2d6a9a3e-5b7c-4d8e-9f01-23456789abcd",
  hostname: "db-1",
  server_id: "7f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02",
  metric: "monitorflow_memory_used_bytes",
  instance: "",
  severity: "warning",
  state: "resolved",
  opened_at: "2026-10-19T09:00:00Z",
  resolved_at: "2026-10-19T09:45:00Z",
  last_value: 1_610_612_736,
  peak_value: 2_147_483_648,
});

describe("IncidentList", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    replace.mockReset();
    search = "";
  });
  afterEach(() => vi.useRealTimers());

  it("lists incidents with readable metrics, values and durations", async () => {
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: {
        status: 200,
        body: { incidents: [open, resolved], next_cursor: null },
      },
    });
    render(<IncidentList filters={NO_FILTERS} onFiltersChange={() => {}} />);
    expect(screen.getByRole("status")).toHaveTextContent("Loading incidents…");

    const rows = within(await screen.findByRole("table", { name: "Incidents" })).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(/◆ Critical\s*web-1 paris\s*Disk used\s*\/var\s*1 hour ago\s*Open\s*1 h\s*96\.2%\s*97\.1%/);
    expect(rows[2]).toHaveTextContent(
      /▲ Warning\s*db-1 paris\s*Memory used\s*3 hours ago\s*Resolved 2 hours ago\s*45 min\s*1\.5 GiB\s*2 GiB/,
    );
    expect(within(rows[1]!).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/view/?\\?id=${SERVER_ID}$`)),
    );
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("follows the cursor and keeps every loaded page on refresh", async () => {
    const page = (n: number, from = 0) =>
      Array.from({ length: n }, (_, i) => incident({ id: `00000000-0000-4000-8000-${String(from + i).padStart(12, "0")}` }));
    const fetchMock = stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: page(PAGE_SIZE), next_cursor: "c1" } },
      [`GET /api/incidents?limit=${PAGE_SIZE}&cursor=c1`]: {
        status: 200,
        body: { incidents: page(10, PAGE_SIZE), next_cursor: null },
      },
      [`GET /api/incidents?limit=${PAGE_SIZE + 10}`]: {
        status: 200,
        body: { incidents: page(PAGE_SIZE + 10), next_cursor: null },
      },
    });
    render(<IncidentList filters={NO_FILTERS} onFiltersChange={() => {}} />);
    await userEvent.click(await screen.findByRole("button", { name: "Load more" }));
    await vi.waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(PAGE_SIZE + 11));
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();

    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(fetchMock.mock.calls.map(([url]) => String(url))).toContain(`/api/incidents?limit=${PAGE_SIZE + 10}`);
    expect(screen.getAllByRole("row")).toHaveLength(PAGE_SIZE + 11);
  });

  it("passes filters to the API and reports changes", async () => {
    stubFetch({
      ...options,
      [`GET /api/incidents?state=open&server_id=${SERVER_ID}&limit=${PAGE_SIZE}`]: {
        status: 200,
        body: { incidents: [], next_cursor: null },
      },
    });
    const onChange = vi.fn();
    const filters = { ...NO_FILTERS, state: "open", serverId: SERVER_ID } as const;
    render(<IncidentList filters={filters} onFiltersChange={onChange} />);
    expect(await screen.findByText("No incidents match these filters.")).toBeInTheDocument();
    expect(await screen.findByRole("option", { name: "web-1" })).toBeInTheDocument();
    expect(screen.getByLabelText("Server")).toHaveValue(SERVER_ID);

    await userEvent.selectOptions(screen.getByLabelText("Severity"), "critical");
    expect(onChange).toHaveBeenLastCalledWith({ ...filters, severity: "critical" });
    await userEvent.selectOptions(screen.getByLabelText("Site"), "paris");
    expect(onChange).toHaveBeenLastCalledWith({ ...filters, siteId: SITE_ID, serverId: null });
    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    expect(onChange).toHaveBeenLastCalledWith(NO_FILTERS);
  });

  it("says when there are no incidents at all", async () => {
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} onFiltersChange={() => {}} />);
    expect(await screen.findByText(/No incidents\. Thresholds have not fired yet/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Clear filters" })).not.toBeInTheDocument();
  });

  it("shows errors and keeps the last list when a refresh fails", async () => {
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: [
        { status: 500, body: { error: "internal error" } },
        { status: 200, body: { incidents: [open], next_cursor: null } },
        { status: 502, body: { error: "bad gateway" } },
      ],
    });
    render(<IncidentList filters={NO_FILTERS} onFiltersChange={() => {}} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByRole("table")).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByRole("alert")).toHaveTextContent(/Refresh failed.*last data/);
    expect(screen.getByRole("table")).toBeInTheDocument();
  });
});

describe("IncidentsView", () => {
  beforeEach(() => replace.mockReset());

  it("reads filters from the query string and writes changes back to it", async () => {
    search = `state=resolved&severity=warning&bogus=1`;
    stubFetch({
      ...options,
      [`GET /api/incidents?state=resolved&severity=warning&limit=${PAGE_SIZE}`]: {
        status: 200,
        body: { incidents: [resolved], next_cursor: null },
      },
    });
    render(<IncidentsView />);
    expect(await screen.findByRole("table")).toBeInTheDocument();
    expect(screen.getByLabelText("State")).toHaveValue("resolved");

    await userEvent.selectOptions(screen.getByLabelText("State"), "open");
    expect(replace).toHaveBeenCalledWith("/incidents/?state=open&severity=warning", { scroll: false });
  });
});
