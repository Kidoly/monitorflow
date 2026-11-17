import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { AddedServer, License, LicenseView } from "./api";

import {
  centralServers,
  coverageNote,
  formatDay,
  licenseNotice,
  licenseSummary,
  supportHref,
  tierIncludes,
} from "./license";

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
    setActiveLocale("fr");
    expect(formatDay("2027-11-30")).toBe("30 novembre 2027");
  });

  it("counts central servers", () => {
    expect(centralServers(1)).toBe("1 central server");
    expect(centralServers(3)).toBe("3 central servers");
  });

  it("says where the license stands in plain words", () => {
    expect(licenseSummary(view("none"))).toEqual(["Community edition", "neutral"]);
    // A key of ADR-0100, without a plan, is Enterprise.
    expect(licenseSummary(view("valid"))).toEqual(["Enterprise subscription, valid until 30 November 2027, for 3 central servers.", "ok"]);
    expect(licenseSummary(view("expires_soon"))[0]).toMatch(
      /^Enterprise subscription, expires on 30 November 2027\. Central servers already added keep running/,
    );
    expect(licenseSummary(view("expired"))).toEqual([
      "Enterprise subscription expired on 30 November 2027. Central servers already added keep running; none can be added until a new license key is installed.",
      "bad",
    ]);
    expect(licenseSummary(view("invalid", { license: null, problem: "the signature does not verify" }))).toEqual([
      "The license key no longer verifies: the signature does not verify.",
      "bad",
    ]);
  });

  it("tells administrators only once a central server was added", () => {
    const added: AddedServer[] = [{ name: "host2", admitted_at: "2026-11-07T10:00:00Z", running: true }];
    expect(licenseNotice(view("none"))).toBeNull();
    expect(licenseNotice(view("expired"))).toBeNull();
    expect(licenseNotice(view("valid", { added }))).toBeNull();
    expect(licenseNotice(view("expires_soon", { added }))).toBe(
      "The Enterprise license expires on 30 November 2027. The central server already added keeps running, but no other can be added after that day.",
    );
    expect(licenseNotice(view("expired", { added: [...added, { name: "host3", admitted_at: "2026-11-07T11:00:00Z", running: false }] }))).toBe(
      "The Enterprise license expired on 30 November 2027. The 2 central servers already added keep running, but no other can be added.",
    );
    expect(licenseNotice(view("none", { added }))).toBe(
      "There is no Enterprise license key. The central server already added keeps running, but no other can be added.",
    );
    setActiveLocale("fr");
    expect(licenseNotice(view("none", { added: [...added, ...added] }))).toBe(
      "Il n’y a pas de clé de licence Enterprise. Les 2 serveurs centraux déjà ajoutés continuent de fonctionner, mais aucun autre ne peut être ajouté.",
    );
    expect(licenseSummary(view("valid"))[0]).toBe("Abonnement Enterprise, valide jusqu’au 30 novembre 2027, pour 3 serveurs centraux.");
  });

  const standard: License = { ...license, format: 2, edition: "core", tier: "standard", managed_servers: 100, server_instances: 1 };

  it("speaks calmly of a plan without central servers (ADR-0147)", () => {
    expect(licenseSummary(view("valid", { license: standard }))).toEqual(["Standard subscription, valid until 30 November 2027.", "ok"]);
    const [soon, soonTone] = licenseSummary(view("expires_soon", { license: standard }));
    expect(soon).toBe(
      "Standard subscription, expires on 30 November 2027. Everything keeps working after that day; the central server is then offered every release as soon as it is published.",
    );
    expect(soonTone).toBe("neutral");
    const [expired, tone] = licenseSummary(view("expired", { license: standard }));
    expect(expired).toMatch(/^Standard subscription expired on 30 November 2027\. Everything keeps working/);
    expect(tone).toBe("neutral");
    // No notice on other pages without added central servers.
    expect(licenseNotice(view("expired", { license: standard }))).toBeNull();
    // Added under an earlier Enterprise key, then a Standard one.
    const added: AddedServer[] = [{ name: "host2", admitted_at: "2026-11-07T10:00:00Z", running: true }];
    expect(licenseNotice(view("valid", { license: standard, added }))).toBe(
      "The Standard subscription does not include the Enterprise edition. The central server already added keeps running, but no other can be added.",
    );
  });

  it("says what each plan includes", () => {
    expect(tierIncludes("basic")).toEqual([
      "Every feature, for any number of servers and users, as in the Community edition",
      "Tested releases for the central server: it is offered a release once it ran longer",
      "Support tickets in business hours, a number per year",
    ]);
    expect(tierIncludes("standard")[2]).toBe("Unlimited support tickets in business hours, with a faster response");
    expect(tierIncludes("premium")).toHaveLength(3);
    expect(tierIncludes("enterprise")[3]).toMatch(/^The Enterprise edition: more than one central server/);
  });

  it("notes calmly when more servers are managed than covered", () => {
    expect(coverageNote(view("valid", { license: standard, managed_servers_in_use: 100 }))).toBeNull();
    expect(coverageNote(view("valid", { license: standard, managed_servers_in_use: 112 }))).toBe(
      "MonitorFlow manages 112 servers and the subscription covers 100. Everything keeps working; your support contact can cover the others at renewal.",
    );
    // A key of ADR-0100 says nothing of managed servers.
    expect(coverageNote(view("valid", { managed_servers_in_use: 112 }))).toBeNull();
    expect(coverageNote(view("expired", { license: standard, managed_servers_in_use: 112 }))).toBeNull();
  });

  it("links support contacts that are addresses only", () => {
    expect(supportHref("support@example.com")).toBe("mailto:support@example.com");
    expect(supportHref("https://example.com/help")).toBe("https://example.com/help");
    expect(supportHref("javascript:alert(1)")).toBeNull();
    expect(supportHref("http://example.com")).toBeNull();
  });
});
