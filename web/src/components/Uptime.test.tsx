import { render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { Uptime, UptimeDay, UptimeRow } from "@/lib/api";
import { browserTimeZone } from "@/lib/onCall";
import { stubFetch } from "@/test/fetch";

import { UptimeSection, toneOf } from "./Uptime";

vi.mock("next/navigation", () => import("@/test/navigation"));

function days(bad: Partial<Record<number, Partial<UptimeDay>>> = {}): UptimeDay[] {
  return Array.from({ length: 90 }, (_, i) => ({
    date: new Date(Date.UTC(2026, 7, 25 + i)).toISOString().slice(0, 10),
    availability: 100,
    down_seconds: 0,
    maintenance_seconds: 0,
    ...bad[i],
  }));
}

function row(overrides: Partial<UptimeRow> = {}): UptimeRow {
  return {
    kind: "server",
    id: "s1",
    name: "mail-1",
    check: null,
    site_name: "paris",
    windows: { day: 100, week: 99.5, month: 99.9, quarter: 97 },
    outages: 2,
    days: days({ 89: { availability: 97.083, down_seconds: 2520 }, 0: { availability: null } }),
    ...overrides,
  };
}

const uptime = (rows: UptimeRow[]): Uptime => ({ at: "2026-11-22T17:00:00Z", time_zone: "Europe/Paris", rows });
const key = (customer?: string) => {
  const params = new URLSearchParams({ time_zone: browserTimeZone() });
  if (customer) params.set("customer_id", customer);
  return `GET /api/availability/uptime?${params.toString()}`;
};

describe("toneOf", () => {
  it("colors a day by how much of it was up", () => {
    expect(toneOf({ availability: 100, down_seconds: 0 })).toBe("up");
    expect(toneOf({ availability: 99.99, down_seconds: 9 })).toBe("minor");
    expect(toneOf({ availability: 98, down_seconds: 1728 })).toBe("major");
    expect(toneOf({ availability: null, down_seconds: 0 })).toBe("none");
  });
});

describe("UptimeSection", () => {
  it("shows the four windows and one bar per day, each with its numbers in words", async () => {
    stubFetch({ [key()]: { status: 200, body: uptime([row(), row({ kind: "check", check: "disk", outages: 1 })]) } });
    render(<UptimeSection customerId={null} />);
    const bars = await screen.findByRole("list", { name: "Daily uptime of mail-1" });
    expect(within(bars).getAllByRole("listitem")).toHaveLength(90);
    const last = within(bars).getAllByRole("listitem")[89]!;
    expect(last).toHaveAccessibleName(/97\.083 %, down 42 min/);
    expect(within(bars).getAllByRole("listitem")[0]).toHaveAccessibleName(/Not measured/);
    expect(screen.getByText("2 outages in 90 days", { exact: false })).toBeInTheDocument();
    expect(screen.getByRole("list", { name: "Daily uptime of mail-1 · disk" })).toBeInTheDocument();
    expect(screen.getAllByText("90 days")[0]!.nextSibling).toHaveTextContent("97.000 %");
    expect(screen.getByRole("list", { name: "Legend" })).toHaveTextContent("Under 99 %");
  });

  it("asks for the reader's customer and time zone, and speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ [key("c1")]: { status: 200, body: uptime([row()]) } });
    render(<UptimeSection customerId="c1" />);
    expect(await screen.findByRole("list", { name: "Disponibilité par jour de mail-1" })).toBeInTheDocument();
    expect(screen.getByText("Disponibilité")).toBeInTheDocument();
  });

  it("says when there is nothing to show", async () => {
    stubFetch({ [key()]: { status: 200, body: uptime([]) } });
    render(<UptimeSection customerId={null} />);
    expect(await screen.findByText(/Nothing to show yet/)).toBeInTheDocument();
  });
});
