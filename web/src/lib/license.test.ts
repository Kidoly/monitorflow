import { describe, expect, it } from "vitest";

import type { License, LicenseView } from "./api";
import { centralServers, formatDay, licenseSummary } from "./license";

const license: License = {
  id: "6f1c1d1e-8a43-4c55-9d43-3c2f6c0e9a01",
  customer: "Example Corp",
  edition: "enterprise",
  server_instances: 3,
  issued: "2026-11-01",
  expires: "2027-11-30",
};

const view = (state: LicenseView["state"], extra: Partial<LicenseView> = {}): LicenseView => ({
  available: true,
  state,
  license: state === "none" ? null : license,
  ...extra,
});

describe("license", () => {
  it("formats days in UTC", () => {
    expect(formatDay("2027-11-30")).toBe("30 November 2027");
    expect(formatDay("2027-01-01")).toBe("1 January 2027");
  });

  it("counts central servers", () => {
    expect(centralServers(1)).toBe("1 central server");
    expect(centralServers(3)).toBe("3 central servers");
  });

  it("says where the license stands in plain words", () => {
    expect(licenseSummary(view("none"))).toEqual(["No license key.", "neutral"]);
    expect(licenseSummary(view("valid"))).toEqual(["Valid until 30 November 2027, for 3 central servers.", "ok"]);
    expect(licenseSummary(view("expires_soon"))[0]).toMatch(/^Expires on 30 November 2027\. Central servers already added keep running/);
    expect(licenseSummary(view("expired"))).toEqual([
      "Expired on 30 November 2027. Central servers already added keep running; none can be added until a new license key is installed.",
      "bad",
    ]);
    expect(licenseSummary(view("invalid", { license: null, problem: "the signature does not verify" }))).toEqual([
      "The license key no longer verifies: the signature does not verify.",
      "bad",
    ]);
  });
});
