import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { niceTicks, segments, timeTicks } from "./chartScale";
import { TimeChart, type TimeSeries } from "./TimeChart";

// 08:30 local time, then one sample every 10 minutes for 6 hours.
const START = new Date(2026, 9, 22, 8, 30).getTime() / 1000;
const END = START + 6 * 3600;
const samples = (f: (i: number) => number | null): [number, number | null][] =>
  Array.from({ length: 37 }, (_, i) => [START + i * 600, f(i)]);

const SERIES: TimeSeries[] = [
  { label: "/var", points: samples((i) => 70 + i * 0.7) },
  { label: "/home", points: samples((i) => (i === 3 ? null : 34)) },
];

const percent = (v: number) => `${v.toLocaleString("en", { maximumFractionDigits: 1 })} %`;

function renderChart() {
  render(
    <TimeChart
      title="Disk used"
      subtitle="Last 6 h"
      series={SERIES}
      start={START}
      end={END}
      format={percent}
      min={0}
      max={100}
      thresholds={[
        { severity: "warning", value: 80, label: "Warning" },
        { severity: "critical", value: 90, label: "Critical" },
      ]}
      markers={[
        { t: START + 3 * 3600 + 600, label: "Opened 11:40", kind: "opened" },
        { t: END, label: "Now · open", kind: "now" },
      ]}
      band={{ start: START + 3 * 3600 + 600, severity: "critical" }}
    />,
  );
}

const live = () => document.querySelector("[aria-live=polite]")!;

describe("TimeChart", () => {
  it("is a figure captioned with its series and thresholds", () => {
    renderChart();
    const figure = screen.getByRole("figure", { name: "Disk used" });
    expect(within(figure).getByRole("heading", { name: "Disk used" })).toBeInTheDocument();
    expect(figure.querySelector("figcaption")).toHaveTextContent(
      "Disk used: /var, /home. Warning 80 %. Critical 90 %. Opened 11:40. Now · open.",
    );
  });

  it("draws nice y ticks, x ticks every sixth, dashed labelled thresholds, markers and the band", () => {
    renderChart();
    const svg = screen.getByRole("group", { name: /Disk used/ });
    const texts = [...svg.querySelectorAll("text")].map((t) => t.textContent);
    expect(texts).toEqual(expect.arrayContaining(["100 %", "75 %", "50 %", "25 %", "0"]));
    expect(texts).toEqual(expect.arrayContaining(["08:30", "09:30", "10:30", "11:30", "12:30", "13:30", "14:30"]));
    const thresholds = screen.getAllByTestId("chart-threshold");
    expect(thresholds.map((t) => t.textContent)).toEqual(["Warning 80 %", "Critical 90 %"]);
    expect(thresholds[0]!.querySelector("line")).toHaveAttribute("stroke-dasharray", "4 3");
    expect(screen.getAllByTestId("chart-marker").map((m) => m.textContent)).toEqual(["Opened 11:40", "Now · open"]);
    expect(screen.getByTestId("chart-band")).toHaveAttribute("opacity", "0.06");
    // The null sample of /home breaks its line in two.
    expect(screen.getAllByTestId("chart-series")[1]!.querySelectorAll("polyline")).toHaveLength(2);
  });

  it("moves a cursor with the keyboard and announces the readout", async () => {
    const user = userEvent.setup();
    renderChart();
    const plot = screen.getByRole("slider", { name: /arrow keys move the cursor/ });
    expect(plot).toHaveAttribute("tabindex", "0");
    await user.tab();
    expect(plot).toHaveFocus();

    await user.keyboard("{ArrowLeft}");
    // The first key puts the cursor on the latest sample.
    expect(live()).toHaveTextContent("14:30:00, /var 95.2 %, /home 34 %");
    await user.keyboard("{ArrowLeft}");
    expect(live()).toHaveTextContent("14:20:00, /var 94.5 %, /home 34 %");
    await user.keyboard("{Home}");
    expect(live()).toHaveTextContent("08:30:00, /var 70 %, /home 34 %");
    await user.keyboard("{ArrowRight}{ArrowRight}{ArrowRight}");
    expect(live()).toHaveTextContent("09:00:00, /var 72.1 %, /home no value");
    expect(plot).toHaveAttribute("aria-valuetext", "09:00:00");
    await user.keyboard("{Escape}");
    expect(live()).toHaveTextContent("");
  });

  it("toggles a series from its legend button without removing it", async () => {
    const user = userEvent.setup();
    renderChart();
    const home = screen.getByRole("button", { name: "/home" });
    expect(home).toHaveAttribute("aria-pressed", "true");
    await user.click(home);
    expect(home).toHaveAttribute("aria-pressed", "false");
    expect(screen.getAllByTestId("chart-series")).toHaveLength(2);
    expect(screen.getAllByTestId("chart-series")[1]!.getAttribute("class")).toMatch(/off/);

    // A hidden series leaves the readout.
    screen.getByRole("slider").focus();
    await user.keyboard("{End}");
    expect(live()).toHaveTextContent("14:30:00, /var 95.2 %");
    expect(live()).not.toHaveTextContent("/home");
  });

  it("draws at most eight series", () => {
    const many = Array.from({ length: 10 }, (_, i) => ({ label: `cpu${i}`, points: samples(() => i) }));
    render(<TimeChart title="CPU" series={many} />);
    expect(screen.getAllByTestId("chart-series")).toHaveLength(8);
    expect(screen.getByText("2 more series not shown.")).toBeInTheDocument();
  });

  it("says when there is nothing to draw", () => {
    render(<TimeChart title="CPU" series={[{ label: "cpu", points: [[START, null]] }]} emptyText="No data yet." />);
    expect(screen.getByText("No data yet.")).toBeInTheDocument();
    expect(screen.queryByRole("slider")).not.toBeInTheDocument();
  });
});

describe("chart scales", () => {
  it("picks five round y ticks", () => {
    expect(niceTicks(0, 100)).toEqual([0, 25, 50, 75, 100]);
    expect(niceTicks(0, 6.8)).toEqual([0, 2, 4, 6, 8]);
    expect(niceTicks(0, 0.9)).toEqual([0, 0.25, 0.5, 0.75, 1]);
    expect(niceTicks(-3, 7)).toEqual([-5, 0, 5, 10, 15]);
    expect(niceTicks(5, 5)).toHaveLength(5);
  });

  it("labels x ticks HH:MM up to a day and DD MMM beyond", () => {
    expect(timeTicks(START, END).map((t) => t.label)).toEqual(["08:30", "09:30", "10:30", "11:30", "12:30", "13:30", "14:30"]);
    const week = timeTicks(START, START + 6 * 86_400).map((t) => t.label);
    expect(week[0]).toBe("22 Oct");
    expect(week[6]).toBe("28 Oct");
  });

  it("breaks lines at nulls and long gaps", () => {
    expect(segments([[0, 1], [10, null], [20, 2], [30, 3], [100, 4]], 10)).toEqual([[[0, 1]], [[20, 2], [30, 3]], [[100, 4]]]);
  });
});
