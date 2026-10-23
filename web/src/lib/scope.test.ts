import { describe, expect, it } from "vitest";

import { scopeFromQuery, scopedHref } from "./scope";
import { hrefWith } from "./urlState";

const ACME = "0b8f3c4e-1a2b-4c3d-8e9f-000000000001";
const PARIS = "5a1e0c2d-0000-4000-8000-000000000001";

describe("scope", () => {
  it("reads the customer and site, ignoring malformed IDs and orphan sites", () => {
    expect(scopeFromQuery(new URLSearchParams(`customer=${ACME}&site=${PARIS}`))).toEqual({ customerId: ACME, siteId: PARIS });
    expect(scopeFromQuery(new URLSearchParams("customer=acme"))).toEqual({ customerId: null, siteId: null });
    expect(scopeFromQuery(new URLSearchParams(`site=${PARIS}`))).toEqual({ customerId: null, siteId: null });
  });

  it("adds the scope to links, keeping their own query", () => {
    expect(scopedHref("/incidents/", { customerId: null, siteId: null })).toBe("/incidents/");
    expect(scopedHref("/incidents/?state=open", { customerId: ACME, siteId: PARIS })).toBe(
      `/incidents/?state=open&customer=${ACME}&site=${PARIS}`,
    );
  });
});

describe("hrefWith", () => {
  it("sets and removes query keys on the current location", () => {
    const location = { pathname: "/servers/view/", search: "?id=s1&tab=sensors", hash: "#top" } as Location;
    expect(hrefWith({ tab: "updates", q: "" }, location)).toBe("/servers/view/?id=s1&tab=updates#top");
    expect(hrefWith({ id: null, tab: null }, location)).toBe("/servers/view/#top");
  });
});
