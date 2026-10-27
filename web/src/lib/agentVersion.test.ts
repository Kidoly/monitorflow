import { describe, expect, it } from "vitest";

import { compareVersions, updateAvailable } from "./agentVersion";

describe("agent versions", () => {
  it("order as semantic versions", () => {
    const ordered = [
      "0.1.0",
      "0.1.1-alpha",
      "0.1.1-alpha.1",
      "0.1.1-beta.2",
      "0.1.1-beta.11",
      "0.1.1",
      "0.2.0",
      "0.10.0",
      "1.0.0",
    ];
    for (let i = 1; i < ordered.length; i++) {
      expect(compareVersions(ordered[i - 1]!, ordered[i]!)).toBeLessThan(0);
      expect(compareVersions(ordered[i]!, ordered[i - 1]!)).toBeGreaterThan(0);
    }
    expect(compareVersions("0.2.0", "0.2.0")).toBe(0);
    expect(compareVersions("v0.2.0", "0.2.0")).toBeNull();
  });

  it("offer an update only to older agents", () => {
    expect(updateAvailable("0.1.0", "0.2.0")).toBe(true);
    expect(updateAvailable("0.2.0", "0.2.0")).toBe(false);
    expect(updateAvailable("0.3.0", "0.2.0")).toBe(false);
    expect(updateAvailable(null, "0.2.0")).toBe(false);
    expect(updateAvailable("0.1.0", null)).toBe(false);
    expect(updateAvailable("dev", "0.2.0")).toBe(false);
  });
});
