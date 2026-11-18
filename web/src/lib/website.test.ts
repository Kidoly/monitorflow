import { describe, expect, it } from "vitest";

import { websiteHost, websiteLabel } from "./website";

describe("website checks in words", () => {
  it("names a site by its host, and its path when not the home page", () => {
    expect(websiteHost("https://shop.test:8443/cart?x=1")).toBe("shop.test:8443");
    expect(websiteLabel("https://shop.test/")).toBe("shop.test");
    expect(websiteLabel("https://shop.test/health/")).toBe("shop.test/health");
    expect(websiteLabel("http://[::1]:8080/a/b")).toBe("[::1]:8080/a/b");
    expect(websiteLabel("not a url")).toBe("not a url");
    expect(websiteLabel(undefined)).toBe("");
  });
});
