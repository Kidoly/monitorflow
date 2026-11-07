import { describe, expect, it } from "vitest";

import { ALL_SITES, customerWide, placeLabel } from "./customers";

describe("customer wording (ADR-0104)", () => {
  it("names the customer of a site only among several", () => {
    expect(placeLabel("acme", "paris", true)).toBe("acme › paris");
    expect(placeLabel("acme", "paris", true, " / ")).toBe("acme / paris");
    expect(placeLabel("acme", "paris", false)).toBe("paris");
    expect(placeLabel(null, "paris", true)).toBe("paris");
  });

  it("calls a customer-wide choice all sites for a single company", () => {
    expect(customerWide("All of acme", true)).toBe("All of acme");
    expect(customerWide("All of acme", false)).toBe(ALL_SITES);
  });
});
