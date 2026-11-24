import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";

import type { Command } from "./api";
import { commandLog, commandSteps, describeCommand, isActive } from "./commands";

const command = (overrides: Partial<Command> = {}): Command => ({
  id: "c1",
  server_id: "s1",
  kind: "apply_updates",
  parameters: { packages: [], security_only: true, allow_reboot: false },
  state: "failed",
  issued_at: "2026-10-15T09:12:00Z",
  expires_at: "2026-10-15T10:12:00Z",
  sent_at: "2026-10-15T09:12:03Z",
  started_at: "2026-10-15T09:12:04Z",
  finished_at: "2026-10-15T09:12:19Z",
  exit_code: 100,
  error: null,
  requested_by: "u1",
  campaign_id: null,
  ...overrides,
});

const clock = (d: Date) => d.toISOString().slice(11, 19);

describe("in French", () => {
  it("names commands and their steps", () => {
    setActiveLocale("fr");
    expect(describeCommand(command())).toEqual({
      title: "Mises à jour de sécurité, sans redémarrage",
      sub: "toutes les mises à jour de sécurité",
    });
    const five = command({ parameters: { packages: ["a", "b", "c", "d", "e"], security_only: false, allow_reboot: true } });
    expect(describeCommand(five)).toEqual({
      title: "Sélection : a, b, c et 2 de plus, redémarrage autorisé",
      sub: "5 paquets",
    });
    expect(commandSteps(command(), clock)).toBe(
      "en file 09:12:00 → envoyée 09:12:03 → en cours 09:12:04 → échouée 09:12:19 (sortie 100)",
    );
  });
});

describe("describeCommand", () => {
  it("names update commands by what they install", () => {
    expect(describeCommand(command())).toEqual({ title: "Security updates, no reboot", sub: "every security update" });
    expect(describeCommand(command({ parameters: { packages: [], security_only: false, allow_reboot: true } }))).toEqual({
      title: "All packages, allow reboot",
      sub: "every upgradable package",
    });
    expect(describeCommand(command({ parameters: { packages: ["nginx"], security_only: false, allow_reboot: false } }))).toEqual({
      title: "Selected: nginx",
      sub: "1 package",
    });
    expect(
      describeCommand(command({ parameters: { packages: ["a", "b", "c", "d", "e"], security_only: false, allow_reboot: false } })).title,
    ).toBe("Selected: a, b, c and 2 more");
  });

  it("names source commands and unknown kinds", () => {
    expect(describeCommand(command({ kind: "configure_sources", parameters: { mode: "MIRROR", repositories: [{}, {}] } }))).toEqual({
      title: "Configure package sources",
      sub: "2 repositories · mirror",
    });
    expect(describeCommand(command({ kind: "revert_sources", parameters: {} })).title).toBe("Revert package sources");
    expect(describeCommand(command({ kind: "reboot", parameters: null })).title).toBe("reboot");
  });

  it("names agent self-updates by version", () => {
    const update = (parameters: Record<string, unknown>) => describeCommand(command({ kind: "self_update", parameters }));
    // What every update does goes in a tooltip, not under each row.
    expect(update({ version: "0.3.0", allow_downgrade: false, health_timeout_seconds: 0 })).toEqual({
      title: "Update agent to 0.3.0",
      sub: "",
      note: "Rolls back unless the new agent connects within 300 s.",
    });
    const downgrade = update({ version: "0.1.0", allow_downgrade: true, health_timeout_seconds: 60 });
    expect(downgrade.sub).toBe("downgrade allowed");
    expect(downgrade.note).toBe("Rolls back unless the new agent connects within 60 s.");
  });
});

describe("command steps", () => {
  it("lists the steps taken, the final state and the exit code", () => {
    expect(commandSteps(command(), clock)).toBe(
      "queued 09:12:00 → sent 09:12:03 → running 09:12:04 → failed 09:12:19 (exit 100)",
    );
    const running = command({ state: "running", finished_at: null, exit_code: null });
    expect(isActive(running)).toBe(true);
    expect(commandSteps(running, clock)).toBe("queued 09:12:00 → sent 09:12:03 → running 09:12:04");
  });

  it("puts the error and the output under the steps", () => {
    const log = commandLog({ ...command({ error: "apt failed" }), stdout: "Reading package lists...\n", stderr: "E: lock\n" }, clock);
    expect(log.split("\n")).toEqual([
      "queued 09:12:00 → sent 09:12:03 → running 09:12:04 → failed 09:12:19 (exit 100)",
      "apt failed",
      "Reading package lists...",
      "E: lock",
    ]);
  });
});

describe("software commands (ADR-0188)", () => {
  it("name the action and the packages", () => {
    setActiveLocale("en");
    const install = command({
      kind: "software",
      parameters: {
        action: "install",
        manager: "winget",
        packages: [{ id: "7zip.7zip", source: "winget" }, { id: "Mozilla.Firefox", source: "winget" }],
        allow_reboot: false,
      },
    });
    expect(describeCommand(install)).toEqual({ title: "Install software: 7zip.7zip, Mozilla.Firefox", sub: "2 packages, no reboot" });
    const remove = command({
      kind: "software",
      parameters: { action: "remove", manager: "winget", packages: [{ id: "7zip.7zip", source: "" }], allow_reboot: true },
    });
    expect(describeCommand(remove).title).toBe("Remove software: 7zip.7zip");
  });
});
