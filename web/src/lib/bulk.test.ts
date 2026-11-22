import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import { ApiError } from "./api";
import { type BulkAction, type BulkOutcome, oncePer, runBulk, summarize, unsettled } from "./bulk";

interface Item {
  id: string;
  name: string;
  writable: boolean;
}

const item = (name: string, writable = true): Item => ({ id: `id-${name}`, name, writable });
const key = (i: Item) => i.id;
const name = (i: Item) => i.name;

function action(act: BulkAction<Item>["act"]): BulkAction<Item> {
  return { done: (count) => `${count} acknowledged`, allowed: (i) => i.writable, act };
}

describe("runBulk", () => {
  it("acts on each item on its own and refuses what the user may not change", async () => {
    const act = vi.fn(async (i: Item) => {
      if (i.name === "db-1") throw new ApiError(403, "forbidden");
      if (i.name === "db-2") throw new ApiError(500, "boom");
      return i.name === "web-3" ? ("skipped" as const) : ("done" as const);
    });
    const items = [item("web-1"), item("web-2", false), item("db-1"), item("db-2"), item("web-3")];
    const outcomes = await runBulk(items, key, action(act));
    // The read-only one never reaches the API.
    expect(act.mock.calls.map(([i]) => i.name)).toEqual(["web-1", "db-1", "db-2", "web-3"]);
    expect(Object.fromEntries(outcomes)).toEqual({
      "id-web-1": { kind: "done" },
      "id-web-2": { kind: "refused" },
      "id-db-1": { kind: "refused" },
      "id-db-2": { kind: "failed", error: "The server could not handle the request. Try again later." },
      "id-web-3": { kind: "skipped" },
    });
    expect(unsettled(outcomes)).toEqual(new Set(["id-web-2", "id-db-1", "id-db-2"]));
  });

  it("keeps at most `concurrency` requests in flight", async () => {
    let running = 0;
    let most = 0;
    const act = async () => {
      running += 1;
      most = Math.max(most, running);
      await new Promise((resolve) => setTimeout(resolve, 1));
      running -= 1;
      return "done" as const;
    };
    const items = Array.from({ length: 10 }, (_, n) => item(`web-${n}`));
    const outcomes = await runBulk(items, key, action(act), 3);
    expect(outcomes.size).toBe(10);
    expect(most).toBe(3);
  });
});

describe("summarize", () => {
  const items = [item("web-1"), item("web-2"), item("web-3"), item("web-4"), item("jellyfin"), item("db-1"), item("db-2")];
  const outcomes = new Map<string, BulkOutcome>([
    ["id-web-1", { kind: "done" as const }],
    ["id-web-2", { kind: "done" as const }],
    ["id-web-3", { kind: "done" as const }],
    ["id-web-4", { kind: "done" as const }],
    ["id-jellyfin", { kind: "refused" as const }],
  ]);

  it("says how many were done and why the others were not, naming them", () => {
    expect(summarize(items, key, name, outcomes, action(vi.fn()))).toEqual({
      text: "4 acknowledged, 1 refused: insufficient rights (jellyfin)",
      tone: "warn",
    });
    const failed = new Map<string, BulkOutcome>([
      ...outcomes,
      ["id-db-1", { kind: "failed" as const, error: "Timed out" }],
      ["id-db-2", { kind: "skipped" as const }],
    ]);
    expect(summarize(items, key, name, failed, { ...action(vi.fn()), skipped: (n) => `${n} already acknowledged` }).text).toBe(
      "4 acknowledged, 1 already acknowledged, 1 refused: insufficient rights (jellyfin), 1 failed: Timed out (db-1)",
    );
  });

  it("names three and counts the rest", () => {
    const refused = new Map(items.map((i) => [i.id, { kind: "refused" as const }]));
    expect(summarize(items, key, name, refused, action(vi.fn())).text).toBe(
      "7 refused: insufficient rights (web-1, web-2, web-3 and 4 more)",
    );
  });

  it("is plain when everything went through", () => {
    const done = new Map(items.slice(0, 2).map((i) => [i.id, { kind: "skipped" as const }]));
    expect(summarize(items, key, name, done, action(vi.fn()))).toEqual({ text: "2 with nothing to do", tone: "ok" });
  });

  it("speaks French", () => {
    setActiveLocale("fr");
    const text = summarize(items, key, name, outcomes, { done: (n) => `${n} pris en charge` }).text;
    expect(text.replace(/ /g, " ")).toBe("4 pris en charge, 1 refusé : droits insuffisants (jellyfin)");
  });
});

describe("oncePer", () => {
  it("calls once per group and shares the outcome", async () => {
    const act = vi.fn(async (i: Item) => (i.name.startsWith("db") ? Promise.reject(new ApiError(403, "no")) : ("done" as const)));
    const items = [item("web-1"), item("web-2"), item("db-1"), item("db-2")];
    const outcomes = await runBulk(items, key, { act: oncePer((i) => i.name.slice(0, 2), act) });
    expect(act).toHaveBeenCalledTimes(2);
    expect([...outcomes.values()].map((o) => o.kind)).toEqual(["done", "done", "refused", "refused"]);
  });
});
