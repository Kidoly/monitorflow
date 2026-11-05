import { describe, expect, it } from "vitest";

import { displayHref, gridRows, isStale, nextPage, staleText, textScale } from "./display";

describe("display", () => {
  it("links to a dashboard or a ready-made one", () => {
    expect(displayHref({ id: "a b" })).toBe("/display/?id=a%20b");
    expect(displayHref({ ready: "overview" })).toBe("/display/?ready=overview");
  });

  it("counts the rows CSS grid places panels on", () => {
    const w = (...widths: (1 | 2 | 3)[]) => widths.map((width) => ({ width }));
    expect(gridRows([])).toBe(1);
    expect(gridRows(w(1, 1, 1))).toBe(1);
    expect(gridRows(w(1, 1, 1, 1))).toBe(2);
    // A wide panel that does not fit the rest of a row starts the next.
    expect(gridRows(w(1, 1, 2, 3))).toBe(3);
    expect(gridRows(w(2, 2, 2))).toBe(3);
  });

  it("shrinks text beyond two rows, never under half", () => {
    expect(textScale(1)).toBe(1);
    expect(textScale(2)).toBe(1);
    expect(textScale(4)).toBeCloseTo(0.65);
    expect(textScale(24)).toBe(0.5);
  });

  it("is stale after twice the refresh period without an answer", () => {
    expect(isStale(null, 1e9, 30_000)).toBe(false);
    expect(isStale(0, 60_000, 30_000)).toBe(false);
    expect(isStale(0, 60_001, 30_000)).toBe(true);
  });

  it("says plainly what the stale data is", () => {
    expect(staleText(new Date(2026, 10, 5, 14, 2, 31))).toBe(
      "Data from 14:02 — this screen has lost its connection to MonitorFlow. It keeps trying.",
    );
  });

  it("goes back to the first page after the last", () => {
    expect(nextPage(0, 3)).toBe(1);
    expect(nextPage(2, 3)).toBe(0);
    expect(nextPage(0, 0)).toBe(0);
  });
});
