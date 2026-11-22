import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { BarList } from "./BarList";
import { Gauge, arc, fraction } from "./Gauge";

const percent = (v: number) => `${Math.round(v * 100)}%`;

describe("Gauge", () => {
  it("places a value on the dial, within its ends", () => {
    expect([fraction(5, 0, 10), fraction(-1, 0, 10), fraction(20, 0, 10), fraction(3, 3, 3)]).toEqual([0.5, 0, 1, 0]);
    // A full dial turns more than half a turn: the large arc flag is set.
    expect(arc(0, 1)).toMatch(/ A 78 78 0 1 1 /);
    expect(arc(0, 0.25)).toMatch(/ A 78 78 0 0 1 /);
  });

  it("writes the value and its ends, in the colour of its band", () => {
    render(
      <Gauge
        value={0.9}
        min={0}
        max={1}
        format={percent}
        color="var(--threshold-crit)"
        bands={[{ from: 0.8, to: 1, color: "var(--threshold-warn)" }]}
        label="CPU: 90%"
      />,
    );
    const dial = screen.getByRole("img", { name: "CPU: 90%" });
    expect(dial).toHaveTextContent("90%0%100%");
    expect(screen.getByTestId("gauge-value")).toHaveAttribute("stroke", "var(--threshold-crit)");
  });

  it("draws no arc without a value", () => {
    render(<Gauge value={null} min={0} max={1} format={percent} color="red" label="CPU" />);
    expect(screen.queryByTestId("gauge-value")).toBeNull();
    expect(screen.getByRole("img")).toHaveTextContent("–");
  });
});

describe("BarList", () => {
  it("fills each bar to its share of the scale", () => {
    render(
      <BarList
        label="Disks"
        min={0}
        max={1}
        format={percent}
        bars={[
          { key: "a", label: "db-1", value: 0.75, color: "red" },
          { key: "b", label: "db-2", value: 2, color: "blue" },
          { key: "c", label: "db-3", value: null, color: "blue" },
        ]}
      />,
    );
    const rows = screen.getAllByRole("listitem");
    expect(rows.map((r) => r.textContent)).toEqual(["db-175%", "db-2200%", "db-3–"]);
    const fills = rows.map((r) => r.querySelector<HTMLElement>("div:last-child > div")!.style.width);
    expect(fills).toEqual(["75%", "100%", "0%"]);
  });
});
