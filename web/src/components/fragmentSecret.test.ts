import { afterEach, describe, expect, it } from "vitest";

import { takeFragmentSecret } from "./fragmentSecret";

describe("takeFragmentSecret", () => {
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("reads the secret once and removes it from the address", () => {
    window.history.replaceState(null, "", "/welcome/#s3cret");
    const held = { current: null as string | null };
    expect(takeFragmentSecret(held)).toBe("s3cret");
    expect(window.location.hash).toBe("");
    expect(window.location.pathname).toBe("/welcome/");
    // A second run, as in development, keeps what the first read.
    expect(takeFragmentSecret(held)).toBe("s3cret");
  });

  it("is empty without one, and leaves the address alone", () => {
    window.history.replaceState(null, "", "/setup/?x=1");
    const held = { current: null as string | null };
    expect(takeFragmentSecret(held)).toBe("");
    expect(window.location.search).toBe("?x=1");
  });
});
