import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import { checkName } from "./alerts";
import type { UsablePlugin } from "./api";
import { CATALOGUE, COMMON, NO_FILTERS, entriesFor, entryFacts, filterEntries, mostUsed, pluginEntries } from "./catalogue";

const plugin: UsablePlugin = {
  name: "http-json",
  kind: "check",
  version: "1.0.0",
  title: "Web address",
  description: "Fetches a web address.",
  capabilities: ["Connects to the address in its URL setting"],
  settings: [{ key: "url", label: "URL", type: "url", required: true }],
  everywhere: true,
  customer_ids: [],
  site_ids: [],
} as UsablePlugin;

const ids = (entries: { id: string }[]) => entries.map((e) => e.id);

describe("the check catalogue", () => {
  it("has unique ids, and common checks that exist", () => {
    expect(new Set(ids(CATALOGUE)).size).toBe(CATALOGUE.length);
    for (const id of COMMON) expect(ids(CATALOGUE)).toContain(id);
  });

  it("has every title, description and hint in French, read when shown", () => {
    setActiveLocale("fr");
    for (const entry of CATALOGUE) {
      expect(entry.title, entry.id).not.toMatch(/^checks\./);
      expect(entry.description, entry.id).not.toMatch(/^checks\./);
      for (const field of entry.type?.fields ?? []) expect(field.hint ?? "", `${entry.id}.${field.key}`).not.toMatch(/^checks\./);
    }
    const ssh = CATALOGUE.find((e) => e.id === "ssh-service")!;
    expect(ssh.title).toBe("Serveur SSH actif");
    expect(ssh.type?.label).toBe("Serveur SSH actif");
    expect(ssh.type?.fields[0]?.hint).toBe("ssh sous Debian et Ubuntu, sshd sous RHEL et Rocky.");
    expect(CATALOGUE.find((e) => e.id === "port")!.title).toBe("Un port répond");
    expect(entryFacts(ssh)).toBe("Linux · Intégré à l’agent");
    expect(ids(filterEntries(CATALOGUE, { ...NO_FILTERS, query: "base de donnees" }))).toContain("postgresql-service");
  });

  it("prefills each template's form with values that pass its own checks", () => {
    for (const entry of CATALOGUE) {
      if (!entry.type) continue;
      for (const field of entry.type.fields) {
        if (field.initial && field.check) expect(field.check(field.initial), `${entry.id}.${field.key}`).toBeNull();
      }
    }
  });

  it("searches every word, without case or accents, in titles, keywords and filter names", () => {
    const find = (query: string) => ids(filterEntries(CATALOGUE, { ...NO_FILTERS, query }));
    expect(find("POSTGRES")).toEqual(["postgresql-service", "postgresql-port"]);
    expect(find("5432")).toEqual(["postgresql-port"]);
    expect(find("databases windows")).toEqual(["mssql-service", "mysql-port", "mssql-port"]);
    expect(find("Réseau")).toEqual([]);
    expect(find("fail2bän")).toEqual(["fail2ban-service"]);
    expect(find("nothing like this")).toEqual([]);
  });

  it("filters by what is monitored, the system and how it works", () => {
    const updates = filterEntries(CATALOGUE, { ...NO_FILTERS, purpose: "updates" });
    expect(ids(updates)).toEqual(["wuauserv-service", "apt-updates"]);
    const proxmoxPorts = filterEntries(CATALOGUE, { ...NO_FILTERS, system: "proxmox", technology: "port" });
    expect(ids(proxmoxPorts)).toContain("proxmox-port");
    expect(ids(proxmoxPorts)).not.toContain("rdp-port");
  });

  it("keeps a server's own system, with its field texts", () => {
    const linux = entriesFor(CATALOGUE, "debian");
    expect(ids(linux)).toContain("pveproxy-service");
    expect(ids(linux)).not.toContain("iis-service");
    const windows = entriesFor(CATALOGUE, "windows");
    expect(ids(windows)).not.toContain("ssh-service");
    expect(windows.find((e) => e.id === "log")!.type!.fields[0]!.placeholder).toBe("C:\\Logs\\app.log");
    expect(entriesFor(CATALOGUE, null)).toBe(CATALOGUE);
  });

  it("adds the check plugins turned on at the site", () => {
    const [entry] = pluginEntries([plugin], "site-1", null);
    expect(entry).toMatchObject({ id: "plugin:http-json", title: "Web address", technology: "plugin" });
    expect(entryFacts(entry!)).toBe("Any system · Plugin");
    expect(pluginEntries([{ ...plugin, everywhere: false }], "site-1", null)).toEqual([]);
  });

  it("ranks by servers running each check, templates by the check they make", () => {
    const https = CATALOGUE.find((e) => e.id === "https-port")!;
    const values = Object.fromEntries(https.type!.fields.map((f) => [f.key, f.initial ?? ""]));
    expect(checkName(https.type!, values)).toBe("port_443");
    const entries = [...CATALOGUE, ...pluginEntries([plugin], "site-1", null)];
    const top = mostUsed(entries, [
      { kind: "port", name: "port_443", plugin: null, servers: 3 },
      { kind: "port", name: "port_9100", plugin: null, servers: 2 },
      { kind: "port", name: "port_9200", plugin: null, servers: 2 },
      { kind: "plugin", name: "http_json_api", plugin: "http-json", servers: 1 },
    ]);
    expect(ids(top)).toEqual(["port", "https-port", "plugin:http-json"]);
    expect(mostUsed(entries, [])).toEqual([]);
  });
});
