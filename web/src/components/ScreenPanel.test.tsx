import { render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { Panel } from "@/lib/api";

import { ScreenPanel } from "./ScreenPanel";

const WINDOW = { start: 1_800_000_000 - 3600, end: 1_800_000_000, step: 60, at: 1_800_000_000 };

const chart: Panel = {
  id: "c",
  title: "Memory",
  kind: "chart",
  width: 2,
  servers: { kind: "all" },
  metric: "monitorflow_memory_used_ratio",
  combine: "avg",
};

// A screen shows what the dashboard page shows (DashboardPanel), larger.
describe("ScreenPanel", () => {
  afterEach(() => setActiveLocale("en"));

  it("names a combined line in the active language", () => {
    setActiveLocale("fr");
    render(
      <ScreenPanel
        panel={chart}
        data={{ id: "c", servers: 3, unavailable: 0, kind: "chart", series: [{ label: "Average", points: [[1_800_000_000, 0.5]] }] }}
        window={WINDOW}
      />,
    );
    expect(screen.getByText("Moyenne")).toBeInTheDocument();
    expect(screen.queryByText("Average")).toBeNull();
  });

  it("leaves out the instance the panel watches", () => {
    render(
      <ScreenPanel
        panel={{ ...chart, kind: "top", instance: "total", order: "highest", count: 5 }}
        data={{ id: "c", servers: 3, unavailable: 0, kind: "top", rows: [{ server_id: "s1", hostname: "db-1", instance: "total", value: 0.5 }] }}
        window={WINDOW}
      />,
    );
    expect(screen.getByRole("listitem")).toHaveTextContent(/^db-150/);
  });

  it("lists servers unreachable behind a parent, and no all-clear then", () => {
    render(
      <ScreenPanel
        panel={{ id: "a", title: "Open alerts", kind: "alerts", width: 3, servers: { kind: "all" }, count: 10 }}
        data={{
          id: "a",
          servers: 3,
          unavailable: 0,
          kind: "alerts",
          alerts: [],
          unreachable: [
            {
              server_id: "s2",
              hostname: "web-1",
              because: { kind: "device", server_id: "d1", relay_id: null, name: "sw-core", site_id: "x", check: null },
              incident_id: "i9",
              since: new Date(WINDOW.at * 1000 - 300_000).toISOString(),
            },
          ],
        }}
        window={WINDOW}
      />,
    );
    expect(screen.getByRole("listitem")).toHaveTextContent(/Unreachable\s*web-1/);
    expect(screen.getByText("sw-core is down")).toBeInTheDocument();
    expect(screen.queryByText(/No alerts|No open alerts/)).toBeNull();
  });
});
