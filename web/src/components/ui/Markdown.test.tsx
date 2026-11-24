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

describe("Markdown for the AI's words", () => {
  const diagnosis = [
    "The root disk is at **91 %**: 4.6 GB of `.deb` files in `/var/cache/apt/archives`.",
    "",
    "1. Clear the cache: `monitorflow-root clean-package-cache`",
    "2. Shrink the journal",
    "",
    "- Logs in /var/log/*.gz and __pycache__ stay as written",
    "# Not a heading",
    "[a link](https://example.com) <b>bold?</b>",
  ].join("\n");

  it("shows bold, inline code, lists and paragraphs", () => {
    const { container } = render(<Markdown text={diagnosis} variant="ai" />);
    expect(container.querySelector("strong")).toHaveTextContent("91 %");
    expect([...container.querySelectorAll("code")].map((c) => c.textContent)).toEqual([
      ".deb",
      "/var/cache/apt/archives",
      "monitorflow-root clean-package-cache",
    ]);
    expect(container.querySelectorAll("ol li")).toHaveLength(2);
    expect(container.querySelector("ul li")).toHaveTextContent("Logs in /var/log/*.gz and __pycache__ stay as written");
    expect(container).not.toHaveTextContent("**");
    expect(container).not.toHaveTextContent("`");
  });

  it("has no italics, headings, links or HTML", () => {
    const { container } = render(<Markdown text={diagnosis} variant="ai" />);
    expect(container.querySelector("em")).toBeNull();
    expect(screen.queryByRole("heading")).toBeNull();
    expect(container).toHaveTextContent("# Not a heading");
    expect(screen.queryByRole("link")).toBeNull();
    expect(container).toHaveTextContent("[a link](https://example.com)");
    expect(container.querySelector("b")).toBeNull();
    expect(container).toHaveTextContent("<b>bold?</b>");
  });

  it("starts the first paragraph with its lead, or puts it before a list", () => {
    const { container } = render(<Markdown text="Free **space**." variant="ai" lead={<strong>Advice:</strong>} />);
    expect(container.querySelectorAll("p")).toHaveLength(1);
    expect(container.querySelector("p")).toHaveTextContent("Advice: Free space.");
    const list = render(<Markdown text={"- one\n- two"} variant="ai" lead="Steps" />).container;
    expect(list.querySelector("p")).toHaveTextContent("Steps");
    expect(list.querySelector("p + ul")).not.toBeNull();
    const empty = render(<Markdown text="" variant="ai" lead="Only" />).container;
    expect(empty).toHaveTextContent("Only");
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
