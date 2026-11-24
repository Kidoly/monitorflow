import { describe, expect, it } from "vitest";

import { EMPTY_QUERY, byteCount, canSearch, changedFields, inventoryCsvHref, inventoryQueryString } from "./inventory";

describe("inventory", () => {
  it("builds the search's query string", () => {
    expect(inventoryQueryString({ kind: "software", q: " openssl ", exact: true, below: "3.0.15", atLeast: "" })).toBe(
      "kind=software&q=openssl&exact=true&version_below=3.0.15",
    );
    expect(inventoryCsvHref({ ...EMPTY_QUERY, kind: "disk" })).toBe("/api/inventory?kind=disk&format=csv");
    expect(inventoryCsvHref(EMPTY_QUERY, "s1")).toBe("/api/inventory?kind=software&server_id=s1&format=csv");
  });

  it("needs two characters to search software only", () => {
    expect(canSearch({ ...EMPTY_QUERY, q: "o" })).toBe(false);
    expect(canSearch({ ...EMPTY_QUERY, q: "op" })).toBe(true);
    expect(canSearch({ ...EMPTY_QUERY, kind: "disk" })).toBe(true);
  });

  it("reads byte counts and changed fields", () => {
    expect(byteCount("1024")).toBe(1024);
    expect(byteCount("")).toBeNull();
    expect(byteCount("x")).toBeNull();
    expect(
      changedFields({ id: 1, at: "", kind: "disk", key: "sda", change: "changed", before: { model: "A", size: "1" }, after: { model: "B", size: "1", serial: "S" } }),
    ).toEqual(["model: A → B", "serial: ∅ → S"]);
  });
});
