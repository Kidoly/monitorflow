import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { LogList, ServerDetail } from "@/lib/api";
import { setUrl } from "@/test/navigation";

import { ServerLogsTab } from "./ServerLogsTab";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a01";
const NOW = new Date("2026-10-22T14:32:05Z");
const HOUR_AGO = new Date(NOW.getTime() - 3_600_000);

const server = { id: ID, hostname: "web-1", os_family: "debian" } as ServerDetail;

const list: LogList = {
  lines: [
    { time: "2026-10-22T14:30:00Z", message: "disk full on /var", priority: 3, unit: "nginx.service", identifier: "nginx", pid: 42 },
    { time: "2026-10-22T14:10:00Z", message: "The agent dropped 12 log lines: they came faster than 10 a second.", priority: 4, unit: null, identifier: "monitorflow", pid: null },
  ],
  more: true,
  units: ["nginx.service", "ssh.service"],
};

/**
 * Stubs `fetch` for the logs route: `answer` gets the query without
 * `start` and `end`, which follow the clock, like "limit=200&level=error".
 * Returns the parsed queries of every request.
 */
function stubLogs(answer: (filters: string) => LogList) {
  const seen: URLSearchParams[] = [];
  const fetchMock = vi.fn<typeof fetch>(async (input) => {
    const url = new URL(String(input), "http://localhost");
    expect(url.pathname).toBe(`/api/servers/${ID}/logs`);
    seen.push(url.searchParams);
    const filters = new URLSearchParams(url.searchParams);
    filters.delete("start");
    filters.delete("end");
    return new Response(JSON.stringify(answer(filters.toString())), {
      status: 200,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return seen;
}

const near = (iso: string | null, date: Date) => Math.abs(Date.parse(iso ?? "") - date.getTime()) < 60_000;

describe("ServerLogsTab", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl(`/servers/view/?id=${ID}&tab=logs`);
  });
  afterEach(() => vi.useRealTimers());

  it("shows the last hour, newest first, with levels in words", async () => {
    const seen = stubLogs(() => list);
    render(<ServerLogsTab server={server} />);
    const table = await screen.findByRole("table", { name: "Logs of web-1" });
    expect(seen[0]!.get("limit")).toBe("200");
    expect(near(seen[0]!.get("start"), HOUR_AGO) && near(seen[0]!.get("end"), NOW)).toBe(true);
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toHaveTextContent(/Error\s*nginx\.service\s*disk full on \/var/);
    // A line of MonitorFlow's own, with no unit, says where the gap is.
    expect(rows[1]).toHaveTextContent(/Warning\s*monitorflow\s*The agent dropped 12 log lines/);
    expect(screen.getByText(/to now, refreshed every 15 seconds/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Show older" })).toBeInTheDocument();
  });

  it("filters by level, unit and text through the URL", async () => {
    const seen = stubLogs((filters) => (filters.includes("unit=") ? { ...list, lines: [], more: false } : list));
    render(<ServerLogsTab server={server} />);
    await screen.findByRole("table", { name: "Logs of web-1" });
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Level" }), "error");
    await vi.waitFor(() => expect(window.location.search).toContain("level=error"));
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Unit" }), "ssh.service");
    expect(await screen.findByText("No line in this time matches the filters.")).toBeInTheDocument();
    const last = seen.at(-1)!;
    expect([last.get("level"), last.get("unit")]).toEqual(["error", "ssh.service"]);
    await userEvent.click(screen.getByRole("button", { name: "Clear filters" }));
    await vi.waitFor(() => expect(window.location.search).not.toContain("level"));
  });

  it("shows the window around an incident, and goes back to the last hour", async () => {
    const from = new Date("2026-10-22T09:45:00Z");
    const to = new Date("2026-10-22T10:30:00Z");
    setUrl(`/servers/view/?id=${ID}&tab=logs&from=${from.toISOString()}&to=${to.toISOString()}`);
    const seen = stubLogs(() => list);
    render(<ServerLogsTab server={server} />);
    await screen.findByRole("table", { name: "Logs of web-1" });
    expect([seen[0]!.get("start"), seen[0]!.get("end")]).toEqual([from.toISOString(), to.toISOString()]);
    expect(screen.getByText(/^From .* to .*\.$/, { exact: false })).not.toHaveTextContent("to now");
    await userEvent.click(screen.getByRole("button", { name: "Show the last hour" }));
    expect(await screen.findByText(/to now, refreshed every 15 seconds/)).toBeInTheDocument();
    expect(window.location.search).not.toContain("from=");
  });

  it("loads older lines on demand", async () => {
    const seen = stubLogs((filters) => (filters === "limit=400" ? { ...list, more: false } : list));
    render(<ServerLogsTab server={server} />);
    await userEvent.click(await screen.findByRole("button", { name: "Show older" }));
    await vi.waitFor(() => expect(screen.queryByRole("button", { name: "Show older" })).not.toBeInTheDocument());
    expect(seen.at(-1)!.get("limit")).toBe("400");
  });

  it("says when nothing was logged", async () => {
    stubLogs(() => ({ lines: [], more: false, units: [] }));
    render(<ServerLogsTab server={server} />);
    expect(await screen.findByText("No warnings or errors in this time")).toBeInTheDocument();
  });

  it("says Windows event logs are not forwarded yet, without asking the API", async () => {
    const seen = stubLogs(() => list);
    render(<ServerLogsTab server={{ ...server, os_family: "windows" }} />);
    expect(screen.getByText("Windows event logs aren't forwarded yet")).toBeInTheDocument();
    expect(seen).toHaveLength(0);
  });
});
