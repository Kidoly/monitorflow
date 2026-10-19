import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { type ChartSeries, MAX_SERIES, TimeSeriesChart, segments, yDomain, yTicks } from "./TimeSeriesChart";

const start = 1_800_000_000;
const end = start + 3600;

function series(label: string, values: (number | null)[]): ChartSeries {
  return { label, points: values.map((v, i) => [start + i * 60, v]) };
}

describe("TimeSeriesChart", () => {
  it("draws one line per series and shows the latest values", () => {
    const { container } = render(
      <TimeSeriesChart
        title="Disk used"
        unit="ratio"
        start={start}
        end={end}
        series={[series("/", [0.5, 0.6, null]), series("/var", [0.1, 0.25])]}
      />,
    );
    expect(screen.getByRole("figure", { name: "Disk used" })).toBeInTheDocument();
    expect(container.querySelectorAll("path")).toHaveLength(2);
    const legend = within(screen.getByRole("list")).getAllByRole("listitem");
    expect(legend[0]).toHaveTextContent("60% /");
    expect(legend[1]).toHaveTextContent("25% /var");
    expect(screen.getByRole("img")).toHaveAccessibleName("Disk used: 60% /, 25% /var");
  });

  it("does not repeat the title for a series named like the chart", () => {
    render(<TimeSeriesChart title="Load" unit="number" start={start} end={end} series={[series("Load", [1.5])]} />);
    expect(screen.getByRole("img")).toHaveAccessibleName("Load: 1.5");
    expect(screen.getByRole("listitem")).toHaveTextContent(/^1\.5$/);
  });

  it("shows the empty text when no series has a value", () => {
    render(
      <TimeSeriesChart title="CPU" unit="ratio" start={start} end={end} series={[series("total", [null])]} emptyText="Nothing yet." />,
    );
    expect(screen.getByText("Nothing yet.")).toBeInTheDocument();
    expect(screen.queryByRole("img")).not.toBeInTheDocument();
  });

  it("draws labelled reference lines and fits them in the scale", () => {
    render(
      <TimeSeriesChart
        title="Load"
        unit="number"
        start={start}
        end={end}
        series={[series("load1", [1, 2])]}
        referenceLines={[
          { value: 4, label: "Warning", severity: "warning" },
          { value: 8, label: "Critical", severity: "critical" },
        ]}
      />,
    );
    const lines = screen.getAllByTestId("reference-line");
    expect(lines.map((l) => l.textContent)).toEqual(["Warning 4", "Critical 8"]);
    expect(yDomain("number", [1, 2], [{ value: 8, label: "", severity: "critical" }])[1]).toBeGreaterThan(8);
  });

  it("shows the values under the pointer", () => {
    render(<TimeSeriesChart title="CPU" unit="ratio" start={start} end={start + 120} series={[series("total", [0.1, 0.2, 0.3])]} />);
    const svg = screen.getByRole("img");
    svg.getBoundingClientRect = () => ({ left: 0, width: 640 }) as DOMRect;
    // x = 64 is the left edge of the plot: the first point.
    fireEvent.pointerMove(svg, { clientX: 64 });
    expect(screen.getByRole("listitem")).toHaveTextContent("10% total");
    expect(screen.getByText(/^At /)).toBeInTheDocument();
    fireEvent.pointerLeave(svg);
    expect(screen.getByRole("listitem")).toHaveTextContent("30% total");
  });

  it("caps the number of series", () => {
    const many = Array.from({ length: MAX_SERIES + 2 }, (_, i) => series(`cpu${i}`, [0.5]));
    render(<TimeSeriesChart title="CPU" unit="ratio" start={start} end={end} series={many} />);
    expect(screen.getAllByRole("listitem")).toHaveLength(MAX_SERIES);
    expect(screen.getByText("2 more series not shown.")).toBeInTheDocument();
  });
});

describe("chart scales", () => {
  it("breaks lines on nulls and on gaps longer than two steps", () => {
    const points: [number, number | null][] = [
      [0, 1],
      [60, 2],
      [120, null],
      [180, 3],
      [600, 4],
    ];
    expect(segments(points, 60)).toEqual([[[0, 1], [60, 2]], [[180, 3]], [[600, 4]]]);
    expect(segments(points)).toEqual([[[0, 1], [60, 2]], [[180, 3], [600, 4]]]);
  });

  it("keeps ratios on 0–100% and labels bytes in round binary units", () => {
    expect(yDomain("ratio", [0.2, 0.4], [])).toEqual([0, 1]);
    expect(yTicks("ratio", 0, 1).map((t) => t.label)).toEqual(["0%", "25%", "50%", "75%", "100%"]);
    expect(yTicks("bytes", 0, 8 * 1024 ** 3).map((t) => t.label)).toEqual([
      "0 GiB",
      "2 GiB",
      "4 GiB",
      "6 GiB",
      "8 GiB",
    ]);
  });
});
