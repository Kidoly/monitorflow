import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { Heatmap } from "./Heatmap";
import { Markdown, safeHref } from "./Markdown";

describe("Markdown", () => {
  it("draws headings, lists, paragraphs and inline marks", () => {
    const { container } = render(
      <Markdown text={"# On call\nCall **Bob** first,\nthen *Alice*.\n\n- Check `df -h`\n- Open [the runbook](https://example.com/run)\n\n1. One\n2. Two"} />,
    );
    expect(screen.getByRole("heading", { name: "On call" })).toBeInTheDocument();
    expect(container.querySelector("strong")).toHaveTextContent("Bob");
    expect(container.querySelector("em")).toHaveTextContent("Alice");
    expect(container.querySelector("p br")).not.toBeNull();
    expect(container.querySelector("ul code")).toHaveTextContent("df -h");
    expect(screen.getByRole("link", { name: "the runbook" })).toHaveAttribute("href", "https://example.com/run");
    expect(container.querySelectorAll("ol li")).toHaveLength(2);
  });

  it("never reads HTML or follows a script link", () => {
    const { container } = render(<Markdown text={'<img src=x onerror="alert(1)"> [x](javascript:alert(1)) [home](/fleet/)'} />);
    expect(container.querySelector("img")).toBeNull();
    expect(container).toHaveTextContent('<img src=x onerror="alert(1)">');
    expect(screen.queryByRole("link", { name: "x" })).toBeNull();
    expect(screen.getByRole("link", { name: "home" })).toHaveAttribute("href", "/fleet/");
    expect([safeHref("//evil.example"), safeHref("mailto:a@b.c"), safeHref("data:text/html,x")]).toEqual([null, "mailto:a@b.c", null]);
  });
});

describe("Heatmap", () => {
  it("draws a row per series, a cell per period coloured by its value", () => {
    render(
      <Heatmap
        label="CPU"
        rows={[{ key: "a", name: "web-1", label: "web-1", points: [[0, 0.2], [60, null], [120, 0.9]] }]}
        start={0}
        end={180}
        step={60}
        format={(v) => `${v * 100}%`}
        color={(v) => ({ background: v > 0.5 ? "red" : "blue", opacity: 1 })}
      />,
    );
    const row = screen.getByRole("row");
    expect(screen.getByRole("rowheader")).toHaveTextContent("web-1");
    expect(screen.getByRole("cell", { name: "web-1: 90%" })).toBeInTheDocument();
    const cells = [...row.querySelectorAll<HTMLElement>("i")];
    expect(cells.map((c) => c.style.background)).toEqual(["blue", "", "red"]);
  });
});
