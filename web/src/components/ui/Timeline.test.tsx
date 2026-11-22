import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Donut } from "./Donut";
import { StateGrid } from "./StateGrid";
import { Timeline, covered } from "./Timeline";

describe("Timeline", () => {
  it("counts overlapping alerts once, within the range", () => {
    expect(
      covered(
        [
          { start: 0, end: 50, severity: "warning" },
          { start: 40, end: 60, severity: "critical" },
          { start: 90, end: 200, severity: "warning" },
        ],
        10,
        100,
      ),
    ).toBe(60);
  });

  it("draws each row's alerts, red over amber, and its clear share", () => {
    render(
      <Timeline
        label="Alerts"
        start={0}
        end={100}
        share={(s) => `${Math.round(s * 100)}% clear`}
        ticks={["a", "b", "c"]}
        rows={[
          {
            key: "s1",
            label: "web-1",
            spans: [
              { start: 50, end: 75, severity: "critical" },
              { start: 0, end: 25, severity: "warning" },
            ],
          },
        ]}
      />,
    );
    const row = screen.getAllByRole("row")[0]!;
    expect(row).toHaveTextContent("web-150% clear");
    const marks = [...row.querySelectorAll<HTMLElement>("i")];
    expect(marks.map((m) => [m.style.left, m.style.width])).toEqual([
      ["0%", "25%"],
      ["50%", "25%"],
    ]);
  });
});

describe("StateGrid and Donut", () => {
  it("names each square and the key", () => {
    render(
      <StateGrid
        label="States"
        squares={[{ key: "a", name: "web-1: Critical", color: "red", href: "/servers/view?id=a" }, { key: "b", name: "web-2: OK", color: "green" }]}
        legend={[{ key: "critical", label: "1 Critical", color: "red" }]}
      />,
    );
    expect(screen.getByRole("link", { name: "web-1: Critical" })).toHaveAttribute("href", "/servers/view?id=a");
    expect(screen.getByRole("img", { name: "web-2: OK" })).toBeInTheDocument();
    expect(screen.getByText("1 Critical")).toBeInTheDocument();
  });

  it("splits the ring by share and names each slice", () => {
    const { container } = render(
      <Donut
        label="States"
        total="4"
        format={String}
        slices={[
          { key: "ok", label: "OK", value: 3, color: "green" },
          { key: "critical", label: "Critical", value: 1, color: "red" },
        ]}
      />,
    );
    expect(screen.getByRole("img", { name: "States" })).toHaveTextContent("4");
    expect(screen.getAllByRole("listitem").map((l) => l.textContent)).toEqual(["OK375%", "Critical125%"]);
    const offsets = [...container.querySelectorAll("circle[stroke]")].map((c) => Number(c.getAttribute("stroke-dashoffset")));
    expect(offsets[0]).toBeCloseTo(0);
    expect(offsets[1]).toBeCloseTo(-0.75 * 2 * Math.PI * 46);
  });
});
