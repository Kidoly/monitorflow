import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { AvailabilityReport, Uptime, UptimeRow } from "@/lib/api";
import { formatAvailability, formatTarget, periodOf } from "@/lib/reports";
import { As, oneSite, viewer } from "@/test/admin";

import { ReportsPage } from "./Reports";

vi.mock("next/navigation", () => import("@/test/navigation"));

const numbers = (down: number, outages: number) => ({
  availability: down ? 100 - (down / 2_678_400) * 100 : 100,
  counted_seconds: 2_678_400,
  down_seconds: down,
  maintenance_seconds: 0,
  unobserved_seconds: 0,
  outages,
  longest_seconds: down,
});

const outage = {
  start: "2026-10-14T03:10:00Z",
  end: "2026-10-14T03:52:00Z",
  seconds: 2520,
  ongoing: false,
  causes: [{ kind: "check" as const, server_id: "s1", server_name: "mail-1", check: "service_postfix", instance: "", via: null }],
};

function report(overrides: Partial<AvailabilityReport> = {}): AvailabilityReport {
  return {
    customer_id: null,
    customer_name: null,
    from: "",
    to: "",
    generated_at: "2026-11-18T10:00:00Z",
    summary: { services: 1, services_met: 0, services_missed: 1, machines: 1, outages: 1, down_seconds: 2520, maintenance_seconds: 7200 },
    services: [
      {
        id: "b1",
        name: "Email",
        customer_id: "c1",
        rule: "all",
        at_least: null,
        target: 99.95,
        met: false,
        budget_seconds: -1180.8,
        members: [{ server_id: "s1", server_name: "mail-1", check: null }],
        numbers: numbers(2520, 1),
        outage_list: [outage],
      },
    ],
    machines: [
      { id: "s1", name: "mail-1", device: false, site_id: "site-1", site_name: "paris", customer_id: "c1", numbers: numbers(2520, 1), outage_list: [outage] },
    ],
    checks: [{ server_id: "s1", server_name: "mail-1", name: "service_postfix", instance: "", numbers: numbers(2520, 1), outage_list: [outage] }],
    maintenance: [{ start: "2026-10-05T01:00:00Z", end: "2026-10-05T03:00:00Z", reason: "Patches", servers: ["mail-1"], checks: [], window: true }],
    ...overrides,
  };
}

function uptimeRow(overrides: Partial<UptimeRow>): UptimeRow {
  return {
    kind: "server",
    id: "s1",
    name: "mail-1",
    check: null,
    site_name: "paris",
    windows: { day: 100, week: 99.5, month: 99.9, quarter: 99.97 },
    outages: 1,
    days: [{ date: "2026-11-18", availability: 100, down_seconds: 0, maintenance_seconds: 0 }],
    ...overrides,
  };
}

/**
 * Answers `GET /api/availability` with `body`, echoing the period asked
 * for, and `GET /api/availability/uptime` with `uptime` when given.
 */
function stubReport(body: AvailabilityReport, uptime?: Uptime) {
  const fetchMock = vi.fn<typeof fetch>(async (input) => {
    const url = new URL(String(input), "http://localhost");
    if (uptime && url.pathname === "/api/availability/uptime") {
      return new Response(JSON.stringify(uptime), { status: 200, headers: { "Content-Type": "application/json" } });
    }
    if (url.pathname !== "/api/availability") throw new Error(`unexpected request ${String(input)}`);
    // As the server writes them: without milliseconds.
    const second = (iso: string | null) => (iso ?? "").replace(/\.\d{3}Z$/, "Z");
    const answer = { ...body, from: second(url.searchParams.get("from")), to: second(url.searchParams.get("to")) };
    return new Response(JSON.stringify(answer), { status: 200, headers: { "Content-Type": "application/json" } });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.unstubAllGlobals();
  setActiveLocale("en");
});

describe("periods", () => {
  const now = new Date(2026, 10, 18, 10, 30);

  it("are months and days in local time", () => {
    expect(periodOf("thisMonth", now)).toEqual({ from: new Date(2026, 10, 1), to: now });
    expect(periodOf("lastMonth", now)).toEqual({ from: new Date(2026, 9, 1), to: new Date(2026, 10, 1) });
    expect(periodOf("last7", now)?.from).toEqual(new Date(now.getTime() - 7 * 86_400_000));
    // Chosen days run to the end of the last one, and not past now.
    expect(periodOf("custom", now, { from: "2026-10-01", to: "2026-10-31" })).toEqual({ from: new Date(2026, 9, 1), to: new Date(2026, 10, 1) });
    expect(periodOf("custom", now, { from: "2026-11-10", to: "2026-11-30" })?.to).toEqual(now);
    expect(periodOf("custom", now, { from: "2026-10-31", to: "2026-10-01" })).toBeNull();
    expect(periodOf("custom", now, { from: "", to: "2026-10-01" })).toBeNull();
  });

  it("write percentages without rounding up to full", () => {
    expect(formatAvailability(99.9996)).toBe("99.999 %");
    expect(formatAvailability(null)).toBe("Not measured");
    expect(formatTarget(99.9)).toBe("99.9 %");
    setActiveLocale("fr");
    expect(formatAvailability(99.95)).toBe("99,950 %");
  });
});

describe("Reports page", () => {
  it("shows the summary first, then the details, and the files to download", async () => {
    const fetchMock = stubReport(report());
    render(
      <As user={oneSite(viewer)}>
        <ReportsPage />
      </As>,
    );
    const summary = await screen.findByRole("region", { name: "Summary" });
    expect(within(summary).getByText("1 server or device watched. 1 outage, 42 min in all.")).toBeInTheDocument();
    expect(within(summary).getByText("2 h")).toBeInTheDocument();
    expect(within(summary).getByText("0 of 1")).toBeInTheDocument();
    const services = screen.getByRole("region", { name: "Business services" });
    expect(within(services).getByText("Missed")).toBeInTheDocument();
    expect(within(services).getByText("99.95 %")).toBeInTheDocument();
    expect(within(services).getByText("Exceeded by 19 min")).toBeInTheDocument();
    const outages = screen.getByRole("region", { name: "Outages" });
    expect(within(outages).getByText("mail-1: check service_postfix")).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Planned maintenance" })).toHaveTextContent("Patches");
    // One site: no site column.
    expect(screen.queryByRole("columnheader", { name: "Site" })).not.toBeInTheDocument();
    const pdf = screen.getByRole("link", { name: "Download PDF" });
    expect(pdf.getAttribute("href")).toMatch(/^\/api\/availability\/report\?from=.*&format=pdf&language=en&time_zone=/);
    expect(String(fetchMock.mock.calls[0]?.[0])).not.toContain("customer_id");
  });

  it("lists each server once: its uptime with the period's numbers under it", async () => {
    const rows = [
      uptimeRow({ kind: "service", id: "b1", name: "Email", site_name: null }),
      uptimeRow({}),
      uptimeRow({ kind: "check", check: "service_postfix" }),
    ];
    stubReport(report(), { at: "2026-11-18T10:00:00Z", time_zone: "Europe/Paris", rows });
    render(
      <As user={oneSite(viewer)}>
        <ReportsPage />
      </As>,
    );
    const lines = await screen.findAllByLabelText("Chosen period: This month");
    expect(lines).toHaveLength(3);
    const [service, machine, check] = lines;
    expect(service).toHaveTextContent("Missed");
    expect(service).toHaveTextContent("Exceeded by 19 min");
    expect(machine).toHaveTextContent("Outages1");
    expect(machine).toHaveTextContent("Maintenance");
    expect(check).toHaveTextContent("Down time42 min");
    // Listed once: no table repeats them, and the downloads stay.
    expect(screen.queryByRole("region", { name: "Servers and devices" })).not.toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Business services" })).not.toBeInTheDocument();
    expect(screen.queryByRole("region", { name: "Checks with outages" })).not.toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Outages" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Download PDF" })).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Download CSV" })).toBeInTheDocument();
  });

  it("asks for chosen days and refuses impossible ones", async () => {
    stubReport(report());
    render(
      <As user={viewer}>
        <ReportsPage />
      </As>,
    );
    await screen.findByRole("region", { name: "Summary" });
    await userEvent.selectOptions(screen.getByLabelText("Period"), "custom");
    const from = screen.getByLabelText("From");
    await userEvent.clear(from);
    await userEvent.type(from, "2099-01-01");
    expect(await screen.findByText("Choose a first day in the past and a last day after it.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Download PDF" })).not.toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubReport(report({ services: [], summary: { ...report().summary, services: 0, outages: 0, down_seconds: 0 } }));
    render(
      <As user={viewer}>
        <ReportsPage />
      </As>,
    );
    const summary = await screen.findByRole("region", { name: "Résumé" });
    expect(within(summary).getByText(/Il n’y a eu aucune panne/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Télécharger le PDF" }).getAttribute("href")).toContain("language=fr");
  });
});
