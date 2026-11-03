import { describe, expect, it } from "vitest";

import type { WindowsUpdateSource } from "@/lib/api";

import { describeUpdateSource } from "./UpdateSource";

const microsoft: WindowsUpdateSource = {
  origin: "microsoft",
  wsus_url: null,
  status_url: null,
  download_url: null,
  managed: false,
  automatic: null,
  no_internet: false,
  quality_from_microsoft: false,
  feature_from_microsoft: false,
  quality_deferral_days: 0,
  feature_deferral_days: 0,
};
const wsus: WindowsUpdateSource = { ...microsoft, origin: "wsus", wsus_url: "http://wsus.example:8530", managed: true };

describe("describeUpdateSource", () => {
  it("says nothing it doesn't know", () => {
    const words = describeUpdateSource(null);
    expect(words.label).toBe("Not known yet");
    expect(words.text).not.toMatch(/WSUS|Microsoft/);
    expect(words.warnings).toEqual([]);
  });

  it("names Microsoft, and says when no policy is set", () => {
    expect(describeUpdateSource(microsoft)).toEqual({
      label: "Microsoft",
      tone: "neutral",
      text: "Updates come from Microsoft (Windows Update).",
      warnings: [],
      details: ["No Windows Update policy is set: Windows uses its own defaults."],
    });
    expect(describeUpdateSource({ ...microsoft, managed: true, automatic: "off" }).details).toEqual([
      "Automatic updates are off.",
    ]);
  });

  it("names the WSUS server and the other servers it uses", () => {
    const words = describeUpdateSource({
      ...wsus,
      status_url: "http://reports.example",
      download_url: "http://cache.example",
      automatic: "install",
    });
    expect(words.label).toBe("WSUS");
    expect(words.text).toBe("Updates come from WSUS at http://wsus.example:8530.");
    expect(words.details).toEqual([
      "Downloads come from http://cache.example.",
      "It reports its status to http://reports.example.",
      "Windows installs updates on its own schedule, outside update campaigns.",
    ]);
  });

  it("warns about a WSUS server named but not turned on", () => {
    const words = describeUpdateSource({ ...microsoft, managed: true, wsus_url: "http://wsus.example:8530" });
    expect(words.text).toBe("Updates come from Microsoft (Windows Update).");
    expect(words.warnings).toEqual([
      "Policy names the WSUS server http://wsus.example:8530 but doesn't turn it on, so Windows ignores it.",
    ]);
  });

  it("warns about WSUS turned on without a server", () => {
    const words = describeUpdateSource({ ...wsus, origin: "wsus_without_url", wsus_url: null });
    expect(words.tone).toBe("warn");
    expect(words.text).toBe("Policy turns WSUS on but names no WSUS server.");
    expect(words.warnings[0]).toMatch(/Specify intranet Microsoft update service location/);
  });

  it("explains updates taken from Microsoft next to WSUS, and the ban that blocks them", () => {
    const words = describeUpdateSource({ ...wsus, quality_from_microsoft: true, no_internet: true, quality_deferral_days: 1 });
    expect(words.details).toEqual([
      "Monthly updates come from Microsoft instead, by policy.",
      "After Microsoft releases them, Windows waits before offering monthly updates 1 day.",
    ]);
    expect(words.warnings).toEqual([
      "Monthly updates come from Microsoft, but policy also forbids Windows Update Internet locations.",
    ]);
    // Without updates from Microsoft, the ban changes nothing worth saying.
    expect(describeUpdateSource({ ...wsus, no_internet: true }).warnings).toEqual([]);
  });

  it("gives the deferrals of updates from Microsoft", () => {
    const words = describeUpdateSource({ ...microsoft, managed: true, quality_deferral_days: 7, feature_deferral_days: 180 });
    expect(words.details).toEqual([
      "After Microsoft releases them, Windows waits before offering monthly updates 7 days and feature updates 180 days.",
    ]);
  });
});
