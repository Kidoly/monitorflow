import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { NO_FILTERS } from "@/lib/incidents";
import { As, me, oneSite, viewer } from "@/test/admin";
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
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open, resolved], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(/^Critical\s*Disk used \/varweb-1 · paris · Acme Hosting\s*09:40 today\s*Open · 4 h 20 min\s*96\.2%\s*peak 97\.1%$/);
    expect(rows[2]).toHaveTextContent(/^was Warning\s*Memory useddb-1 · berlin · Northwind\s*Yesterday 23:10\s*Resolved 01:42 · 2 h 32 min\s*1\.5 GiB\s*peak 2 GiB$/);
    expect(within(rows[1]!).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/servers/view/?\\?id=${SERVER_ID}$`)),
    );
    expect(within(rows[1]!).getByRole("link", { name: "Disk used /var" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/incidents/view/?\\?id=${open.id}$`)),
    );
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("says an incident is in maintenance, or silenced, until when", async () => {
    const until = new Date(2026, 9, 19, 16, 0).toISOString();
    const quiet = incident({ silenced_until: until, in_maintenance: true });
    const hushed = incident({ id: "3e7b0b4f-0000-4000-8000-000000000009", silenced_until: until, in_maintenance: false });
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [quiet, hushed], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows[1]).toHaveTextContent(/In maintenance\s*until 16:00 today/);
    expect(rows[2]).toHaveTextContent(/Silenced\s*until 16:00 today/);
  });

  it("names no site while there is only one", async () => {
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open], next_cursor: null } },
    });
    render(
      <As user={oneSite(viewer)}>
        <IncidentList filters={NO_FILTERS} />
      </As>,
    );
    const rows = within(await table()).getAllByRole("row");
    expect(rows[1]).toHaveTextContent(/Disk used \/varweb-1\s*09:40 today/);
    expect(screen.queryByRole("combobox", { name: "Site" })).toBeNull();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open, resolved], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    const rows = within(await screen.findByRole("table", { name: "Incidents" })).getAllByRole("row");
    expect(rows[0]).toHaveTextContent(/Gravité\s*Incident\s*Début\s*État\s*Dernière valeur/);
    expect(rows[1]).toHaveTextContent(/aujourd’hui 09:40\s*Ouvert · 4 h 20 min\s*96,2 %\s*pic 97,1 %/);
    expect(rows[2]).toHaveTextContent(/^était Avertissement/);
    expect(rows[2]!.textContent!.match(/Résolu/g)).toHaveLength(1);
    expect(rows[2]).toHaveTextContent(/Résolu 01:42/);
  });

  it("loads 50 more and keeps every loaded incident on refresh", async () => {
    const page = (n: number) => Array.from({ length: n }, (_, i) => incident({ id: `00000000-0000-4000-8000-${String(i).padStart(12, "0")}` }));
    const fetchMock = stubFetch({
      ...options,
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

  it("lists what one notification sent together, and the way back to all", async () => {
    const GROUP = "9a3e2c1b-7d4f-4e6a-8b5c-1d2e3f4a5b6c";
    setUrl(`/incidents/?group=${GROUP}`);
    stubFetch({
      ...options,
      [`GET /api/incidents?group=${GROUP}&limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open, resolved], next_cursor: null } },
    });
    render(<IncidentList filters={{ ...NO_FILTERS, groupId: GROUP }} />);
    await vi.waitFor(async () => expect(within(await table()).getAllByRole("row")).toHaveLength(3));
    expect(screen.getByText(/Sent together in one notification/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Show all incidents" })).toHaveAttribute("href", expect.stringMatching(/^\/incidents\/?$/));
  });

  it("acknowledges one incident, or all of a notification's at once", async () => {
    const GROUP = "9a3e2c1b-7d4f-4e6a-8b5c-1d2e3f4a5b6c";
    setUrl(`/incidents/?group=${GROUP}`);
    const mine = incident({ can_acknowledge: true });
    const other = incident({ id: "3d6a9a3e-5b7c-4d8e-9f01-23456789abcd", hostname: "web-2", can_acknowledge: true });
    const done = incident({
      id: "4d6a9a3e-5b7c-4d8e-9f01-23456789abcd",
      hostname: "web-3",
      acknowledged_by: "bob",
      acknowledged_at: new Date(2026, 9, 19, 13, 50).toISOString(),
      acknowledged_note: "swapping the disk",
    });
    const fetchMock = stubFetch({
      ...options,
      [`GET /api/incidents?group=${GROUP}&limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [mine, other, done], next_cursor: null } },
      "POST /api/incidents/acknowledge": { status: 200, body: { acknowledged: [] } },
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(<IncidentList filters={{ ...NO_FILTERS, groupId: GROUP }} />);
    await vi.waitFor(async () => expect(within(await table()).getAllByRole("row")).toHaveLength(4));
    expect(screen.getByText(/Acknowledged by bob 13:50: swapping the disk/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Acknowledge web-2" }));
    await user.type(screen.getByRole("textbox", { name: "What are you doing about it?" }), "on it");
    await user.click(screen.getByRole("button", { name: "Acknowledge" }));
    await user.click(screen.getByRole("button", { name: "Acknowledge all 2" }));
    // Escape closes the note without acknowledging; the button gets focus back.
    await user.keyboard("{Escape}");
    expect(screen.getByRole("button", { name: "Acknowledge all 2" })).toHaveFocus();
    await user.click(screen.getByRole("button", { name: "Acknowledge all 2" }));
    await user.type(screen.getByRole("textbox", { name: "What are you doing about it?" }), "restarting{Enter}");
    const bodies = fetchMock.mock.calls
      .filter(([url]) => url === "/api/incidents/acknowledge")
      .map(([, init]) => JSON.parse(String(init?.body)));
    expect(bodies).toEqual([
      { incident_ids: [other.id], note: "on it" },
      { incident_ids: [mine.id, other.id], note: "restarting" },
    ]);
  });

  it("acts on the checked incidents, each on its own, and says how it went", async () => {
    const a = incident({ id: "1a6a9a3e-5b7c-4d8e-9f01-23456789abcd", hostname: "web-a", can_acknowledge: true });
    const b = incident({ id: "1b6a9a3e-5b7c-4d8e-9f01-23456789abcd", hostname: "web-b", can_acknowledge: true });
    // Read-only for this user: refused without a request.
    const c = incident({ id: "1c6a9a3e-5b7c-4d8e-9f01-23456789abcd", hostname: "web-c", can_acknowledge: false });
    const d = incident({
      id: "1d6a9a3e-5b7c-4d8e-9f01-23456789abcd",
      hostname: "web-d",
      acknowledged_by: "bob",
      acknowledged_at: new Date(2026, 9, 19, 13, 50).toISOString(),
    });
    const fetchMock = stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [a, b, c, d], next_cursor: null } },
      "POST /api/incidents/acknowledge": [
        { status: 200, body: { acknowledged: [a.id] } },
        { status: 500, body: { error: "internal error", code: "internal" } },
      ],
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(<IncidentList filters={NO_FILTERS} />);
    await vi.waitFor(async () => expect(within(await table()).getAllByRole("row")).toHaveLength(5));
    const bar = screen.getByRole("region", { name: "Bulk actions" });
    expect(within(bar).queryByRole("button")).not.toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: "Select Disk used /var on web-a" }));
    expect(within(bar).getByText("1 selected")).toBeInTheDocument();
    await user.click(screen.getByRole("checkbox", { name: "Select every row shown" }));
    expect(within(bar).getByText("4 selected")).toBeInTheDocument();

    await user.click(within(bar).getByRole("button", { name: "Acknowledge the 4 selected incidents" }));
    await user.type(within(bar).getByRole("textbox", { name: "What are you doing about it?" }), "on it{Enter}");
    expect(await within(bar).findByRole("status")).toHaveTextContent(
      /^1 acknowledged, 1 already acknowledged or resolved, 1 refused: insufficient rights \(Disk used \/var on web-c\), 1 failed: The server could not handle the request\. Try again later\. \(Disk used \/var on web-b\)/,
    );
    const bodies = fetchMock.mock.calls
      .filter(([url]) => url === "/api/incidents/acknowledge")
      .map(([, init]) => JSON.parse(String(init?.body)));
    expect(bodies).toEqual([
      { incident_ids: [a.id], note: "on it" },
      { incident_ids: [b.id], note: "on it" },
    ]);
    // What did not go through stays checked, to try again.
    expect(within(bar).getByText("2 selected")).toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "Select Disk used /var on web-b" })).toBeChecked();
    expect(screen.getByRole("checkbox", { name: "Select Disk used /var on web-a" })).not.toBeChecked();
    await user.click(within(bar).getByRole("button", { name: "Deselect" }));
    expect(screen.getByRole("checkbox", { name: "Select every row shown" })).not.toBeChecked();
    await user.click(within(bar).getByRole("button", { name: "Hide" }));
    expect(within(bar).getByRole("status")).toBeEmptyDOMElement();
  });

  it("silences the servers of the checked incidents, once per server", async () => {
    const RELAY = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a09";
    const a = incident({ id: "2a6a9a3e-5b7c-4d8e-9f01-23456789abcd" });
    const b = incident({ id: "2b6a9a3e-5b7c-4d8e-9f01-23456789abcd", metric: "monitorflow_memory_used_bytes", instance: "" });
    const relayDown = incident({ id: "2c6a9a3e-5b7c-4d8e-9f01-23456789abcd", server_id: null, relay_id: RELAY, hostname: "relay-1", metric: "offline", instance: "" });
    const over = incident({ id: "2d6a9a3e-5b7c-4d8e-9f01-23456789abcd", state: "resolved", resolved_at: NOW.toISOString() });
    const berlin = incident({ id: "2e6a9a3e-5b7c-4d8e-9f01-23456789abcd", hostname: "db-1", server_id: "7f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02", site_id: OTHER_SITE, site_name: "berlin" });
    const fetchMock = stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [a, b, relayDown, over, berlin], next_cursor: null } },
      "POST /api/silences": { status: 201, body: {} },
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    // An operator of paris only: berlin's server is refused without a request.
    render(
      <As user={me({ role: "operator", customer_id: null, site_id: SITE_ID })}>
        <IncidentList filters={NO_FILTERS} />
      </As>,
    );
    await vi.waitFor(async () => expect(within(await table()).getAllByRole("row")).toHaveLength(6));
    await user.click(screen.getByRole("checkbox", { name: "Select every row shown" }));
    const bar = screen.getByRole("region", { name: "Bulk actions" });
    await user.click(within(bar).getByRole("button", { name: "Silence the servers of the 5 selected incidents" }));
    await user.click(screen.getByRole("menuitem", { name: "1 hour" }));
    expect(await within(bar).findByRole("status")).toHaveTextContent(
      /^3 silenced, 1 already resolved, 1 refused: insufficient rights \(Disk used \/var on db-1\)/,
    );
    const bodies = fetchMock.mock.calls
      .filter(([url]) => url === "/api/silences")
      .map(([, init]) => JSON.parse(String(init?.body)));
    expect(bodies).toEqual([
      { scope: "server", target_id: SERVER_ID, ends_at: expect.any(String), reason: "Planned maintenance" },
      { scope: "relay", target_id: RELAY, ends_at: expect.any(String), reason: "Planned maintenance" },
    ]);
    // An hour from now; the fake clock moves on a little while the test clicks.
    expect(Date.parse(bodies[0].ends_at) - NOW.getTime()).toBeGreaterThanOrEqual(3_600_000);
    expect(Date.parse(bodies[0].ends_at) - NOW.getTime()).toBeLessThan(3_660_000);
  });

  it("silences for a custom period", async () => {
    const fetchMock = stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open], next_cursor: null } },
      "POST /api/silences": { status: 403, body: { error: "forbidden", code: "forbidden" } },
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(
      <As user={me({ role: "operator", customer_id: null, site_id: null })}>
        <IncidentList filters={NO_FILTERS} />
      </As>,
    );
    await user.click(await screen.findByRole("checkbox", { name: "Select Disk used /var on web-1" }));
    await user.click(screen.getByRole("button", { name: "Silence the server of the selected incident" }));
    await user.click(screen.getByRole("menuitem", { name: "Custom…" }));
    const reason = screen.getByRole("textbox", { name: "Why" });
    await user.clear(reason);
    await user.type(reason, "disk swap");
    await user.type(screen.getByLabelText(/^Until/), "2026-10-19T18:00");
    await user.click(screen.getByRole("button", { name: "Silence their servers" }));
    // The API has the last word on rights.
    expect(await screen.findByRole("status")).toHaveTextContent(/^1 refused: insufficient rights \(Disk used \/var on web-1\)/);
    const [body] = fetchMock.mock.calls.filter(([url]) => url === "/api/silences").map(([, init]) => JSON.parse(String(init?.body)));
    expect(body).toMatchObject({ scope: "server", target_id: SERVER_ID, reason: "disk swap", ends_at: new Date(2026, 9, 19, 18).toISOString() });
  });

  it("keeps the scope's customer only", async () => {
    setUrl(`/incidents/?customer=${ACME}`);
    stubFetch({
      ...options,
      [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [open, resolved], next_cursor: null } },
    });
    render(<IncidentList filters={NO_FILTERS} />);
    await vi.waitFor(async () => expect(within(await table()).getAllByRole("row")).toHaveLength(2));
    expect(screen.getByRole("link", { name: "web-1" })).toHaveAttribute("href", expect.stringContaining(`customer=${ACME}`));
  });

  it("says when there are no incidents at all", async () => {
    stubFetch({ ...options, [`GET /api/incidents?limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [], next_cursor: null } } });
    render(<IncidentList filters={NO_FILTERS} />);
    expect(await screen.findByText(/No incidents\. No threshold has fired/)).toBeInTheDocument();
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
      [`GET /api/incidents?state=resolved&severity=warning&limit=${PAGE_SIZE}`]: { status: 200, body: { incidents: [resolved], next_cursor: null } },
    });
    render(<IncidentsView />);
    expect(await table()).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "State" })).toHaveValue("resolved");
  });
});
