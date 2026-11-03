import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Dashboard, DashboardData, Panel, PanelData, Server } from "@/lib/api";
import { READY_MADE } from "@/lib/dashboards";
import { NO_SCOPE } from "@/lib/scope";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { router, setUrl } from "@/test/navigation";

import { DashboardPanel, emptyReason } from "./DashboardPanel";
import { DashboardView, DashboardsPage } from "./Dashboards";
import { PanelForm } from "./PanelForm";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02";
const viewer = me({ role: "viewer", customer_id: "c1", site_id: null });

const chart: Panel = {
  id: "c",
  title: "CPU of the web servers",
  kind: "chart",
  width: 2,
  servers: { kind: "all" },
  metric: "monitorflow_cpu_usage_ratio",
  instance: "total",
  combine: "each",
};

const WINDOW = { start: 1_800_000_000, end: 1_800_003_600, step: 30, at: 1_800_003_600 };

function data(panels: PanelData[]): DashboardData {
  return { ...WINDOW, panels };
}

function dashboard(overrides: Partial<Dashboard> = {}): Dashboard {
  return {
    id: ID,
    name: "Web",
    shared_with: { kind: "customer", id: "c1", name: "acme" },
    panels: 1,
    updated_at: "2026-11-01T10:00:00Z",
    updated_by_username: "alice",
    can_edit: true,
    spec: { version: 1, range: "6h", panels: [chart] },
    revision: 3,
    ...overrides,
  };
}

function server(id: string, hostname: string, siteId = "site-1"): Server {
  return {
    id,
    peer_id: hostname,
    hostname,
    site_id: siteId,
    site_name: siteId === "site-1" ? "paris" : "lyon",
    customer_id: "c1",
    customer_name: "acme",
    relay_id: null,
    os_family: "debian",
    os_version: "12",
    agent_version: "0.2.0",
    created_at: "2026-10-01T00:00:00Z",
    revoked_at: null,
    status: "online",
    connected_at: null,
    last_heartbeat: null,
    via_relay: null,
    open_incidents: 0,
    cert_not_after: null,
  };
}

const bodyOf = (fetchMock: ReturnType<typeof stubFetch>, key: string) => {
  const [method, path] = key.split(" ");
  const call = fetchMock.mock.calls.filter((c) => String(c[0]) === path && (c[1]?.method ?? "GET") === method).pop();
  return JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;
};

beforeEach(() => router.push.mockClear());
afterEach(() => vi.unstubAllGlobals());

describe("DashboardPanel", () => {
  const render1 = (panel: Panel, panelData: PanelData) =>
    render(<DashboardPanel panel={panel} data={panelData} window={WINDOW} scope={NO_SCOPE} />);

  it("draws a chart of each server with values in the metric's unit", () => {
    render1(chart, {
      id: "c",
      servers: 2,
      unavailable: 0,
      kind: "chart",
      series: [
        { label: "web-1", server_id: "s1", points: [[1_800_000_000, 0.5]] },
        { label: "web-2", server_id: "s2", points: [[1_800_000_000, 0.25]] },
      ],
    });
    expect(screen.getByRole("figure", { name: "CPU of the web servers" })).toBeInTheDocument();
    expect(screen.getByText("2 servers")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "web-1" })).toBeInTheDocument();
  });

  it("shows a figure, a top list and alerts with links", () => {
    const { unmount } = render1(
      { ...chart, kind: "figure", combine: "count", title: "Servers" },
      { id: "c", servers: 12, unavailable: 0, kind: "figure", value: 11 },
    );
    expect(screen.getByText("11")).toBeInTheDocument();
    expect(screen.getByText("Now, of 12 servers")).toBeInTheDocument();
    unmount();

    const top = render1(
      { ...chart, kind: "top", metric: "monitorflow_disk_used_ratio", order: "highest", count: 5, title: "Fullest disks" },
      { id: "c", servers: 3, unavailable: 0, kind: "top", rows: [{ server_id: "s1", hostname: "db-1", instance: "/var", value: 0.931 }] },
    );
    expect(screen.getByRole("link", { name: "db-1" })).toHaveAttribute(
      "href",
      "/servers/sensor?id=s1&sensor=disk_used_ratio&instance=%2Fvar",
    );
    expect(screen.getByText("93.1%")).toBeInTheDocument();
    top.unmount();

    render1(
      { id: "a", title: "Open alerts", kind: "alerts", width: 3, servers: { kind: "all" }, count: 10 },
      {
        id: "a",
        servers: 3,
        unavailable: 0,
        kind: "alerts",
        alerts: [
          {
            id: "i1",
            server_id: "s1",
            hostname: "db-1",
            metric: "monitorflow_disk_used_ratio",
            instance: "/var",
            severity: "critical",
            opened_at: new Date(WINDOW.at * 1000 - 600_000).toISOString(),
          },
        ],
      },
    );
    expect(screen.getByText("Critical")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Disk used /var" })).toHaveAttribute("href", "/incidents/view?id=i1");
    expect(screen.getByText("on db-1")).toBeInTheDocument();
  });

  it("says why it is empty or failed", () => {
    const named: Panel = { ...chart, servers: { kind: "list", ids: ["gone"] } };
    const none = { id: "c", servers: 0, unavailable: 1, kind: "chart", series: [] } as PanelData;
    expect(emptyReason(named, none)).toMatch(/removed, or you cannot see them/);
    expect(emptyReason(chart, { ...none, unavailable: 0 })).toBe("No servers here yet.");
    render1(chart, { id: "c", servers: 0, unavailable: 0, kind: "error", error: "The metrics backend timed out. Try a shorter range." });
    expect(screen.getByRole("alert")).toHaveTextContent("timed out");
  });

  it("notes chosen servers it cannot show", () => {
    render1(
      { ...chart, kind: "figure", combine: "avg" },
      { id: "c", servers: 1, unavailable: 2, kind: "figure", value: 0.5 },
    );
    expect(screen.getByText("2 of the chosen servers are removed or not visible to you.")).toBeInTheDocument();
  });
});

describe("PanelForm", () => {
  it("builds a panel from lists, the title following the choices", async () => {
    stubFetch({
      "GET /api/dashboard-metrics": { status: 200, body: { metrics: ["monitorflow_cpu_usage_ratio", "monitorflow_disk_used_ratio"], instances: [] } },
      "GET /api/dashboard-metrics?metric=monitorflow_cpu_usage_ratio": {
        status: 200,
        body: { metrics: [], instances: ["cpu0", "total"] },
      },
      "GET /api/dashboard-metrics?metric=monitorflow_disk_used_ratio": { status: 200, body: { metrics: [], instances: ["/", "/var"] } },
    });
    const onDone = vi.fn();
    const start: Panel = { id: "n", title: "", kind: "chart", width: 2, servers: { kind: "all" }, metric: "monitorflow_cpu_usage_ratio", combine: "each" };
    render(
      <PanelForm
        initial={start}
        isNew
        servers={[server("s1", "web-1"), server("s2", "db-1", "site-2")]}
        within={{ kind: "all" }}
        onDone={onDone}
        onCancel={() => {}}
      />,
    );
    // The total is watched by default once the instances are known.
    expect(await screen.findByRole("option", { name: "total" })).toBeInTheDocument();
    expect(screen.getByLabelText("Watches")).toHaveValue("total");

    await userEvent.click(screen.getByLabelText("Top list"));
    await userEvent.selectOptions(screen.getByLabelText("Metric"), "monitorflow_disk_used_ratio");
    await userEvent.selectOptions(await screen.findByLabelText("Watches"), "/var");
    expect(screen.getByLabelText("Title")).toHaveValue("Highest: Disk used /var");
    await userEvent.selectOptions(screen.getByLabelText("Servers"), "list");
    await userEvent.click(screen.getByRole("button", { name: "Add panel" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Choose at least one server.");
    await userEvent.click(screen.getByLabelText(/db-1/));
    await userEvent.click(screen.getByRole("button", { name: "Add panel" }));
    expect(onDone).toHaveBeenCalledWith({
      id: "n",
      title: "Highest: Disk used /var",
      kind: "top",
      width: 2,
      servers: { kind: "list", ids: ["s2"] },
      metric: "monitorflow_disk_used_ratio",
      instance: "/var",
      order: "highest",
      count: 5,
    });
  });
});

describe("Dashboards", () => {
  it("lists the ready-made dashboards and the user's own", async () => {
    setUrl("/dashboards/");
    stubFetch({ "GET /api/dashboards": { status: 200, body: { dashboards: [dashboard()] } } });
    render(
      <As user={viewer}>
        <DashboardsPage />
      </As>,
    );
    expect(await screen.findByRole("link", { name: "Web" })).toHaveAttribute("href", `/dashboards/view?id=${ID}`);
    expect(screen.getByText("Customer acme")).toBeInTheDocument();
    for (const d of READY_MADE) expect(screen.getByRole("link", { name: new RegExp(d.name) })).toBeInTheDocument();
  });

  it("shows a ready-made dashboard within the chosen customer, and copies it", async () => {
    const customer = "22222222-2222-4222-8222-222222222222";
    setUrl(`/dashboards/view/?ready=overview&customer=${customer}`);
    const overview = READY_MADE[0]!;
    const fetchMock = stubFetch({
      "POST /api/dashboard-data": {
        status: 200,
        body: data(overview.spec.panels.map((p) => ({ id: p.id, servers: 2, unavailable: 0, kind: "figure", value: null }) as PanelData)),
      },
      "POST /api/dashboards": { status: 201, body: dashboard({ id: "11111111-1111-4111-8111-111111111111" }) },
    });
    render(
      <As user={viewer}>
        <DashboardView />
      </As>,
    );
    expect(await screen.findByRole("heading", { name: "Servers reporting" })).toBeInTheDocument();
    expect(screen.getByText("Ready-made")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(bodyOf(fetchMock, "POST /api/dashboard-data")).toMatchObject({ range: "6h", within: { kind: "customer", id: customer } });

    await userEvent.click(screen.getByRole("button", { name: "Make a copy" }));
    const created = bodyOf(fetchMock, "POST /api/dashboards");
    expect(created).toMatchObject({ name: "Copy of Fleet overview", shared_with: { kind: "personal" } });
    // "Every server" of the copy stays the customer the original showed.
    const panels = (created.spec as { panels: Panel[] }).panels;
    expect(panels.every((p) => p.servers.kind === "customer")).toBe(true);
    expect(router.push).toHaveBeenCalledWith(`/dashboards/view/?id=11111111-1111-4111-8111-111111111111&customer=${customer}`);
  });

  it("changes the range in the address only", async () => {
    setUrl(`/dashboards/view/?id=${ID}`);
    const fetchMock = stubFetch({
      [`GET /api/dashboards/${ID}`]: { status: 200, body: dashboard() },
      "POST /api/dashboard-data": { status: 200, body: data([{ id: "c", servers: 1, unavailable: 0, kind: "chart", series: [] }]) },
    });
    render(
      <As user={viewer}>
        <DashboardView />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "7 d" }));
    expect(window.location.search).toContain("range=7d");
    await vi.waitFor(() => expect(bodyOf(fetchMock, "POST /api/dashboard-data").range).toBe("7d"));
  });

  it("edits, arranges and saves with the revision, and says when someone saved first", async () => {
    setUrl(`/dashboards/view/?id=${ID}`);
    const second: Panel = { id: "a", title: "Alerts", kind: "alerts", width: 1, servers: { kind: "list", ids: ["s1", "gone"] }, count: 5 };
    const fetchMock = stubFetch({
      [`GET /api/dashboards/${ID}`]: { status: 200, body: dashboard({ spec: { version: 1, range: "6h", panels: [chart, second] } }) },
      "POST /api/dashboard-data": { status: 200, body: data([]) },
      "GET /api/servers": { status: 200, body: { servers: [server("s1", "web-1")], next_cursor: null } },
      [`PUT /api/dashboards/${ID}`]: [
        { status: 409, body: { error: "Someone changed this dashboard since you opened it. Reload it to see their changes." } },
        { status: 200, body: dashboard({ revision: 4, name: "Web and alerts" }) },
      ],
    });
    render(
      <As user={customerAdmin}>
        <DashboardView />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Edit" }));
    // The removed server leaves the alerts panel.
    expect(await screen.findByText(/1 removed server was taken out/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Move Alerts earlier" }));
    await userEvent.selectOptions(screen.getByLabelText("Width of Alerts"), "3");
    const name = screen.getByLabelText("Name");
    await userEvent.clear(name);
    await userEvent.type(name, "Web and alerts");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText(/Someone changed this dashboard/)).toBeInTheDocument();
    const body = bodyOf(fetchMock, `PUT /api/dashboards/${ID}`);
    expect(body).toMatchObject({ name: "Web and alerts", revision: 3, shared_with: { kind: "customer", id: "c1" } });
    const panels = (body.spec as { panels: Panel[] }).panels;
    expect(panels.map((p) => [p.id, p.width])).toEqual([
      ["a", 3],
      ["c", 2],
    ]);
    expect(panels[0]!.servers).toEqual({ kind: "list", ids: ["s1"] });

    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("heading", { name: "Web and alerts" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
  });

  it("adds a panel to a new dashboard and creates it", async () => {
    setUrl("/dashboards/view/?new=1");
    const fetchMock = stubFetch({
      "GET /api/servers": { status: 200, body: { servers: [server("s1", "web-1")], next_cursor: null } },
      "GET /api/dashboard-metrics": { status: 200, body: { metrics: ["monitorflow_cpu_usage_ratio"], instances: [] } },
      "GET /api/dashboard-metrics?metric=monitorflow_cpu_usage_ratio": { status: 200, body: { metrics: [], instances: ["total"] } },
      "POST /api/dashboard-data": { status: 200, body: data([]) },
      "POST /api/dashboards": { status: 201, body: dashboard({ name: "Mine", shared_with: { kind: "personal" } }) },
    });
    render(
      <As user={viewer}>
        <DashboardView />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Add a panel" }));
    const form = await screen.findByRole("form", { name: "New panel" });
    await within(form).findByRole("option", { name: "total" });
    await userEvent.click(within(form).getByRole("button", { name: "Add panel" }));
    await userEvent.type(screen.getByLabelText("Name"), "Mine");
    // A viewer keeps it for itself: the only sharing offered.
    expect(within(screen.getByLabelText(/^Shared with/)).getAllByRole("option").map((o) => o.textContent)).toEqual(["Only me"]);
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    await vi.waitFor(() => expect(router.replace).toHaveBeenCalledWith(`/dashboards/view/?id=${ID}`));
    expect(bodyOf(fetchMock, "POST /api/dashboards")).toEqual({
      name: "Mine",
      shared_with: { kind: "personal" },
      spec: {
        version: 1,
        range: "6h",
        panels: [expect.objectContaining({ kind: "chart", metric: "monitorflow_cpu_usage_ratio", instance: "total", title: "CPU usage" })],
      },
    });
  });

  it("deletes after a confirmation naming it", async () => {
    setUrl(`/dashboards/view/?id=${ID}`);
    const fetchMock = stubFetch({
      [`GET /api/dashboards/${ID}`]: { status: 200, body: dashboard() },
      "POST /api/dashboard-data": { status: 200, body: data([]) },
      [`DELETE /api/dashboards/${ID}`]: { status: 204 },
    });
    render(
      <As user={customerAdmin}>
        <DashboardView />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Delete" }));
    await userEvent.click(screen.getByRole("button", { name: "Delete Web" }));
    await vi.waitFor(() => expect(router.push).toHaveBeenCalledWith("/dashboards/"));
    expect(fetchMock.mock.calls.some((c) => c[1]?.method === "DELETE")).toBe(true);
  });
});
