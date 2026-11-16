import { describe, expect, it } from "vitest";

import type { BackupsView, InstallKeyList, IntegrationEndpoint, LicenseView, ServerUpdateView } from "@/lib/api";
import { authority } from "@/test/certificateAuthority";
import { relay } from "@/test/admin";
import { setActiveLocale } from "@/i18n/locale";

import {
  addServersStatus,
  attention,
  authorityLine,
  backupsStatus,
  integrationsStatus,
  licenseStatus,
  notificationsStatus,
  relaysStatus,
  serverUpdateStatus,
  sitesStatus,
} from "./status";

const NOW = new Date("2026-11-14T10:00:00");

describe("settings status lines", () => {
  it("makes no notification channel a problem, which puts a badge on Settings", () => {
    const none = notificationsStatus([]);
    expect(none).toEqual({ text: "No channel yet", tone: "bad" });
    expect(attention({ notifications: none })).toEqual({ count: 1, tone: "bad" });
    const one = { last_error: null } as Parameters<typeof notificationsStatus>[0][number];
    expect(notificationsStatus([one])).toEqual({ text: "1 channel", tone: "neutral" });
  });

  it("counts sites, and customers only for someone who sees several", () => {
    const site = (customer_id: string) => ({ customer_id }) as Parameters<typeof sitesStatus>[0][number];
    expect(sitesStatus([site("a"), site("a"), site("b")], true).text).toBe("2 customers · 3 sites");
    expect(sitesStatus([site("a")], false).text).toBe("1 site");
  });

  it("puts relays waiting for confirmation, then offline ones, before the count", () => {
    expect(relaysStatus([])).toBeUndefined();
    expect(relaysStatus([relay(), relay()])).toEqual({ text: "2 relays connected", tone: "ok" });
    expect(relaysStatus([relay(), relay({ status: "offline" })])).toEqual({ text: "1 of 2 relays connected", tone: "warn" });
    expect(relaysStatus([relay({ pending_names: ["r"] })])?.text).toBe("1 relay waits for confirmation");
    expect(relaysStatus([relay({ revoked_at: "2026-11-01T00:00:00Z", status: "offline" })])).toBeUndefined();
    setActiveLocale("fr");
    expect(relaysStatus([relay(), relay()])?.text).toBe("2 relais connectés");
    expect(relaysStatus([relay({ pending_names: ["r"] })])?.text).toBe("1 relais attend une confirmation");
  });

  it("asks for approval of waiting servers", () => {
    const list = (pending: number, active: number) =>
      ({ pending_hosts: Array(pending).fill({}), keys: Array(active).fill({ state: "active" }) }) as InstallKeyList;
    expect(addServersStatus(list(2, 1))).toEqual({ text: "2 servers wait for approval", tone: "warn" });
    expect(addServersStatus(list(0, 1))).toEqual({ text: "1 install command in use", tone: "neutral" });
  });

  it("says how long the certificate authority lasts, and when machines need a new install", () => {
    expect(authorityLine(authority()).tone).toBe("ok");
    expect(authorityLine(authority()).text).toMatch(/^Valid until \d{4}-\d{2}-\d{2}$/);
    const cutOff = authorityLine(authority({ cut_off: [{} as never, {} as never] }));
    expect(cutOff).toEqual({ text: "2 machines need a new install", tone: "bad" });
  });

  it("treats a turned-off integration as critical and a failing one as a warning", () => {
    const endpoint = (e: Partial<IntegrationEndpoint>) => ({ state: "on", failing_since: null, ...e }) as IntegrationEndpoint;
    const list = (...endpoints: IntegrationEndpoint[]) => ({ endpoints, events: [] });
    expect(integrationsStatus(list())).toBeUndefined();
    expect(integrationsStatus(list(endpoint({}), endpoint({ state: "disabled" })))?.tone).toBe("bad");
    expect(integrationsStatus(list(endpoint({ failing_since: "2026-11-14T09:00:00Z" })))?.tone).toBe("warn");
    expect(integrationsStatus(list(endpoint({})))).toEqual({ text: "1 integration", tone: "neutral" });
  });

  it("tells an available release, a failed update, and the version otherwise", () => {
    const view = (v: Partial<ServerUpdateView>) =>
      ({ running_version: "0.3.0", update: null, requested: false, available: null, ...v }) as ServerUpdateView;
    expect(serverUpdateStatus(view({}))).toEqual({ text: "Up to date, 0.3.0", tone: "ok" });
    const available = { version: "0.3.1", release_notes_url: null, first_seen_at: "" };
    expect(serverUpdateStatus(view({ available }))).toEqual({ text: "0.3.1 available", tone: "warn" });
    expect(serverUpdateStatus(view({ update: { to: "0.3.1", state: "failed" } as never })).tone).toBe("bad");
    expect(serverUpdateStatus(view({ requested: true })).text).toBe("Updating…");
  });

  it("tells the last backup, a late one and a failed one", () => {
    const view = (v: Partial<BackupsView>) => ({ enabled: true, last: null, last_success_at: null, ...v }) as BackupsView;
    expect(backupsStatus(view({ enabled: false }), NOW).tone).toBe("neutral");
    expect(backupsStatus(view({ last_success_at: "2026-11-14T03:00:00" }), NOW)).toEqual({
      text: "Last backup 03:00 today, OK",
      tone: "ok",
    });
    expect(backupsStatus(view({ last_success_at: "2026-11-12T03:00:00" }), NOW).tone).toBe("warn");
    expect(backupsStatus(view({ last: { state: "failed" } as never }), NOW).tone).toBe("bad");
    expect(backupsStatus(view({}), NOW)).toEqual({ text: "No backup yet", tone: "warn" });
  });

  it("asks about the license only once a central server was added", () => {
    const license = { id: "l", customer: "x", edition: "e", server_instances: 2, issued: "2026-01-01", expires: "2026-11-30" };
    const view = (state: LicenseView["state"], added: number): LicenseView => ({
      available: true,
      state,
      license,
      added: Array(added).fill({ name: "b", admitted_at: "", running: true }),
    });
    expect(licenseStatus({ available: false })).toBeUndefined();
    expect(licenseStatus(view("valid", 0))).toEqual({ text: "Valid until 30 November 2026", tone: "ok" });
    expect(licenseStatus(view("expired", 0))?.tone).toBe("neutral");
    expect(licenseStatus(view("expired", 1))?.tone).toBe("bad");
    expect(licenseStatus(view("none", 0))).toEqual({ text: "No license key", tone: "neutral" });
  });

  it("counts what needs someone, critical first", () => {
    expect(attention({ sites: { text: "", tone: "neutral" }, ai: { text: "", tone: "ok" } })).toBeNull();
    expect(attention({ relays: { text: "", tone: "warn" } })).toEqual({ count: 1, tone: "warn" });
    expect(attention({ relays: { text: "", tone: "warn" }, backups: { text: "", tone: "bad" } })).toEqual({
      count: 2,
      tone: "bad",
    });
  });
});
