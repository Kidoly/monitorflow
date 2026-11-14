import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Reason } from "./Reason";
import { Sparkline } from "./Sparkline";

describe("Sparkline", () => {
  it("draws the values with a gap where one is missing", () => {
    render(<Sparkline points={[[0, 0.1], [60, 0.5], [120, null], [180, 0.2], [240, 0.3]]} start={0} end={240} ratio tone="bad" label="CPU usage, last hour" />);
    const svg = screen.getByRole("img", { name: "CPU usage, last hour" });
    const d = svg.querySelector("path")!.getAttribute("d")!;
    expect(d.match(/M/g)).toHaveLength(2);
    expect(d.startsWith("M0.0 ")).toBe(true);
    expect(svg.getAttribute("class")).toMatch(/bad/);
  });

  it("shows a dash with fewer than two values", () => {
    render(<Sparkline points={[[0, 1]]} start={0} end={60} label="x" />);
    expect(screen.queryByRole("img")).toBeNull();
    expect(screen.getByText("–")).toBeInTheDocument();
  });
});

describe("Reason", () => {
  it("keeps the whole sentence in the tooltip when short", () => {
    render(<Reason short text="9% available is below the warning limit of 12%" />);
    expect(screen.getByText("9% available is below the warning limit of 12%")).toHaveAttribute("title", "9% available is below the warning limit of 12%");
  });

  it("renders nothing without a reason", () => {
    const { container } = render(<Reason text={null} />);
    expect(container).toBeEmptyDOMElement();
  });
});
