import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { MaintenanceWindow } from "@/lib/api";
import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { requestOf } from "./MaintenanceWindows";
import { SilenceBar, SilencesPage, type SilenceTarget } from "./Silences";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });
const target: SilenceTarget = { scope: "server", id: "s1", name: "web-1", customerId: "c1", siteId: "site-1" };
const ok = (body: unknown) => ({ status: 200, body });
const places = {
  "GET /api/sites": ok({ sites: [{ id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme", server_count: 2, relay_count: 0 }] }),
  "GET /api/servers": ok({
    servers: [
      { id: "s1", hostname: "web-1", site_id: "site-1", customer_id: "c1" },
      { id: "s2", hostname: "db-1", site_id: "site-1", customer_id: "c1" },
    ],
    next_cursor: null,
  }),
  "GET /api/customers": ok({ customers: [{ id: "c1", name: "acme" }] }),
  "GET /api/sites/site-1/sensors": ok({ site_id: "site-1", check_interval_ms: 60_000, sensors: [] }),
  "GET /api/servers/s1/sensors": ok({
    server_id: "s1",
    site_id: "site-1",
    revision: 1,
    applied_revision: 1,
    applied_at: null,
    in_sync: true,
    check_interval_ms: 60_000,
    sensors: [
      { origin: "builtin", entry_id: null, kind: "cpu", name: "cpu", instance: "", interval_ms: 60_000, timeout_ms: null, params: {} },
      { origin: "server", entry_id: "e1", kind: "systemd", name: "service_nginx", instance: "", interval_ms: 60_000, timeout_ms: null, params: {} },
    ],
  }),
};

function window(overrides: Partial<MaintenanceWindow> = {}): MaintenanceWindow {
  return {
    id: "w1",
    site_id: "site-1",
    site_name: "paris",
    customer_id: "c1",
    servers: [],
    checks: null,
    repeat: "weekly",
    every: 1,
    weekdays: ["sunday"],
    month_day: null,
    month_week: null,
    months: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    start_time: "02:00",
    duration_minutes: 120,
    time_zone: "Europe/Paris",
    starts_on: "2026-11-18",
    ends_on: null,
    reason: "Patch night",
    active_until: null,
    next: [
      { starts_at: "2026-11-22T01:00:00Z", ends_at: "2026-11-22T03:00:00Z" },
      { starts_at: "2026-11-29T01:00:00Z", ends_at: "2026-11-29T03:00:00Z" },
    ],
    created_by_username: "alice",
    created_at: "2026-11-18T09:00:00Z",
    updated_at: "2026-11-18T09:00:00Z",
    can_change: true,
    ...overrides,
  };
}

const bodyOf = (fetchMock: ReturnType<typeof stubFetch>, method: string) =>
  JSON.parse(String(fetchMock.mock.calls.find((c) => c[1]?.method === method)?.[1]?.body)) as Record<string, unknown>;

afterEach(() => {
  vi.useRealTimers();
  setActiveLocale("en");
});

describe("maintenance windows", () => {
  it("lists windows in plain words with their next window, on the Silences page", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date("2026-11-18T10:00:00Z"));
    stubFetch({
      ...places,
      "GET /api/silences": ok({ silences: [] }),
      "GET /api/maintenance-windows": ok({
        windows: [
          window(),
          window({
            id: "w2",
            servers: [{ id: "s1", hostname: "web-1" }],
            checks: [{ name: "service_nginx", instance: "" }],
            repeat: "monthly",
            weekdays: ["tuesday"],
            month_week: 2,
            start_time: "22:00",
            duration_minutes: 240,
            reason: "Database work",
            active_until: "2026-11-18T12:00:00Z",
            can_change: false,
          }),
        ],
      }),
    });
    render(
      <As user={operator}>
        <SilencesPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Maintenance windows" });
    const [, weekly, monthly] = within(table).getAllByRole("row");
    expect(weekly).toHaveTextContent("All of site paris");
    expect(weekly).toHaveTextContent("Every Sunday, 02:00 to 04:00 (Europe/Paris)");
    expect(weekly).toHaveTextContent("Next: Sunday 22 November, 02:00 to 04:00");
    expect(within(weekly!).getByRole("button", { name: "Actions for the maintenance window Patch night" })).toBeInTheDocument();
    expect(monthly).toHaveTextContent("web-1: service_nginx");
    expect(monthly).toHaveTextContent("On the second Tuesday of every month, 22:00 to 02:00 the next day (Europe/Paris)");
    expect(monthly).toHaveTextContent(/In maintenance until/);
    expect(within(monthly!).queryByRole("button")).not.toBeInTheDocument();
  });

  it("makes a window for one server and its checks from the server's menu", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...places,
      "GET /api/silences?server_id=s1": ok({ silences: [] }),
      "GET /api/maintenance-windows?server_id=s1": [ok({ windows: [] }), ok({ windows: [window({ servers: [{ id: "s1", hostname: "web-1" }] })] })],
      "POST /api/maintenance-windows": { status: 201, body: window() },
    });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    await user.click(await screen.findByRole("button", { name: "Silence notifications about web-1" }));
    await user.click(screen.getByRole("menuitem", { name: "Maintenance window…" }));
    const form = await screen.findByRole("form", { name: "New maintenance window" });
    expect(within(form).getByRole("radio", { name: "Chosen servers" })).toBeChecked();
    expect(await within(form).findByRole("checkbox", { name: "web-1" })).toBeChecked();
    await user.click(await within(form).findByRole("checkbox", { name: "service_nginx" }));
    await user.click(within(form).getByRole("checkbox", { name: "Saturday" }));
    // 22:00 to 02:00 ends the next day.
    await user.clear(within(form).getByLabelText("From"));
    await user.type(within(form).getByLabelText("From"), "22:00");
    await user.clear(within(form).getByLabelText("To"));
    await user.type(within(form).getByLabelText("To"), "02:00");
    expect(within(form).getByLabelText("Ends")).toHaveValue("1");
    expect(form).toHaveTextContent("In words: Every Saturday and Sunday, 22:00 to 02:00 the next day");
    await user.click(within(form).getByRole("button", { name: "Add the maintenance window" }));
    await vi.waitFor(() =>
      expect(bodyOf(fetchMock, "POST")).toMatchObject({
        site_id: "site-1",
        server_ids: ["s1"],
        checks: [{ name: "service_nginx", instance: "" }],
        repeat: "weekly",
        weekdays: ["sunday", "saturday"],
        start_time: "22:00",
        duration_minutes: 240,
      }),
    );
    expect(await screen.findByText(/Maintenance window: Every Sunday, 02:00 to 04:00/)).toBeInTheDocument();
  });

  it("removes a window after confirming", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...places,
      "GET /api/silences": ok({ silences: [] }),
      "GET /api/maintenance-windows": [ok({ windows: [window()] }), ok({ windows: [] })],
      "DELETE /api/maintenance-windows/w1": { status: 204 },
    });
    render(
      <As user={operator}>
        <SilencesPage />
      </As>,
    );
    await user.click(await screen.findByRole("button", { name: "Actions for the maintenance window Patch night" }));
    await user.click(screen.getByRole("menuitem", { name: "Remove…" }));
    expect(screen.getByText("Remove the maintenance window Patch night?")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await vi.waitFor(() => expect(fetchMock.mock.calls.some((c) => c[1]?.method === "DELETE")).toBe(true));
    expect(await screen.findByText(/No maintenance window/)).toBeInTheDocument();
  });

  it("offers viewers no way to add one", async () => {
    stubFetch({ ...places, "GET /api/silences": ok({ silences: [] }), "GET /api/maintenance-windows": ok({ windows: [] }) });
    render(
      <As user={viewer}>
        <SilencesPage />
      </As>,
    );
    expect(await screen.findByText(/No maintenance window/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "New maintenance window" })).not.toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date("2026-11-18T10:00:00Z"));
    stubFetch({ ...places, "GET /api/silences": ok({ silences: [] }), "GET /api/maintenance-windows": ok({ windows: [window()] }) });
    render(
      <As user={operator}>
        <SilencesPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Fenêtres de maintenance" });
    expect(table).toHaveTextContent("Chaque dimanche, de 02:00 à 04:00 (Europe/Paris)");
    expect(table).toHaveTextContent(/Prochaine\s*:\s*dimanche 22 novembre, de 02:00 à 04:00/);
  });

  it("refuses a window without length or over a week before asking the server", () => {
    const draft = {
      siteId: "site-1", wholeSite: true, serverIds: [], checks: [], repeat: "weekly" as const, every: 1,
      weekdays: ["sunday" as const], byWeek: false, monthDay: 1, monthWeek: 1, months: [], from: "02:00", to: "02:00",
      endsAfter: 0, zone: "UTC", startsOn: "", endsOn: "", reason: "x",
    };
    expect(requestOf(draft)).toBe("The end must differ from the start.");
    expect(requestOf({ ...draft, endsAfter: 7, to: "03:00" })).toBe("A window lasts at most 7 days.");
    expect(requestOf({ ...draft, to: "04:00", wholeSite: false })).toBe("Choose at least one server, or the whole site.");
    expect(requestOf({ ...draft, to: "04:00", repeat: "monthly", byWeek: true, weekdays: ["friday", "monday"], monthWeek: 5 })).toMatchObject({
      weekdays: ["friday"],
      month_week: 5,
      month_day: null,
      duration_minutes: 120,
    });
  });
});
