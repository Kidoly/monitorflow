import { describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { event } from "@/test/remediation";

import {
  actorLabel,
  auditFiltersFromQuery,
  auditQuery,
  chainSummary,
  eventSummary,
  riskWord,
  rollbackText,
  snapshotReason,
  timeLeft,
} from "./remediation";

describe("eventSummary", () => {
  it("describes each kind from its payload", () => {
    const cases: [Partial<Parameters<typeof eventSummary>[0]>, string][] = [
      [
        { kind: "context_sent", payload: { model: "claude/opus", context: { redactions: { password: 2, token: 1 } } } },
        "claude/opus, 3 secrets redacted",
      ],
      [{ kind: "ai_proposal", payload: { diagnosis: "disk full", command: "df -h" } }, "df -h · disk full"],
      [
        { kind: "ai_proposal", payload: { diagnosis: "fixed", command: null, stop: "resolved", reason: "usage dropped" } },
        "Stopped (considers it fixed): usage dropped",
      ],
      [
        { kind: "classification", payload: { tier: "low_impact", rule: { source: "builtin", id: "low.systemctl" }, reason: "restarts a unit" } },
        "Low-impact by rule low.systemctl: restarts a unit",
      ],
      [
        { kind: "classification", payload: { tier: "read_only", rule: { source: "builtin", id: "readonly.free" }, reason: "`free` only reports system state" } },
        "Read-only by rule readonly.free: free only reports system state",
      ],
      [{ kind: "approval", actor_kind: "system", payload: { command: "df -h" } }, "df -h"],
      [{ kind: "approval", actor_kind: "user", payload: { command: "df -h" } }, "df -h"],
      [{ kind: "edit", payload: { original: "a", edited: "b" } }, "a → b"],
      [{ kind: "rejection", payload: { command: "reboot", reason: "not now" } }, "reboot · not now"],
      [{ kind: "rejection", payload: { command: "reboot", reason: null } }, "reboot"],
      [{ kind: "snapshot", payload: { method: "none", name: null, detail: "rollback is limited" } }, "None: rollback is limited"],
      [{ kind: "snapshot", payload: { method: "proxmox", name: "mf-1-2", detail: "" } }, "mf-1-2 (proxmox)"],
      [{ kind: "command_queued", payload: { command: "df -h" } }, "df -h"],
      [{ kind: "command_result", payload: { exit_code: 0, duration_ms: 1500 } }, "Exit code 0, 1.5 s"],
      [{ kind: "command_result", payload: { exit_code: null, error: "expired", duration_ms: 20 } }, "Did not complete: expired, 20 ms"],
      // Written before closing codes (ADR-0144): the server's sentence, without the event's name again.
      [{ kind: "escalation", payload: { reason: "limit" } }, "limit"],
      [{ kind: "state_change", payload: { from: "awaiting_approval", to: "running" } }, "awaiting approval → running"],
      [{ kind: "note", payload: { text: "the sensor returned to normal" } }, "the sensor returned to normal"],
      [
        { kind: "note", payload: { text: "the sensor returned to normal", code: "sensor_normal", from: "running", to: "resolved" } },
        "The sensor returned to normal",
      ],
      [
        { kind: "escalation", payload: { reason: "the duration limit of 2 s was reached", code: "duration_limit", limit: 2, to: "escalated" } },
        "The duration limit of 2 seconds was reached",
      ],
      [
        { kind: "escalation", payload: { reason: "the AI provider failed: timeout", code: "ai_failed", detail: "timeout", to: "failed" } },
        "Failed: The AI provider failed: timeout",
      ],
      // The result just before says why: the escalation does not quote it again.
      [
        { kind: "escalation", payload: { reason: "the command did not run to completion: refused", code: "incomplete", to: "escalated" } },
        "The command did not complete",
      ],
      // A code this page does not know yet: the server's English.
      [{ kind: "escalation", payload: { reason: "a new reason", code: "from_the_future" } }, "a new reason"],
      [
        { kind: "command_result", payload: { exit_code: null, error: "no result within 30 s", error_code: "no_result", limit: 30, duration_ms: 20 } },
        "Did not complete: no result within 30 seconds, 20 ms",
      ],
      [
        {
          kind: "command_result",
          payload: { exit_code: null, error: "shell commands are disabled on this agent (commands.shell); run the installer with MF_ALLOW_COMMANDS=1 to allow them" },
        },
        "Did not complete: AI commands are off on this agent. Run the installer with MF_ALLOW_COMMANDS=1 to allow them.",
      ],
    ];
    for (const [overrides, want] of cases) {
      expect(eventSummary(event(overrides))).toBe(want);
    }
  });

  it("speaks French when that is the language", () => {
    setActiveLocale("fr");
    expect(eventSummary(event({ kind: "command_result", payload: { exit_code: 0, duration_ms: 1500 } }))).toBe(
      "Code de sortie 0, 1,5 s",
    );
    expect(eventSummary(event({ kind: "state_change", payload: { from: "awaiting_approval", to: "running" } }))).toBe(
      "en attente d’approbation → en cours",
    );
    // MonitorFlow's own messages are translated from their codes, the AI's words kept as they are.
    expect(eventSummary(event({ kind: "escalation", payload: { reason: "x", code: "commands_off", to: "escalated" } }))).toBe(
      "Les commandes IA sont désactivées sur ce serveur. Relancez-y l’installateur avec MF_ALLOW_COMMANDS=1 pour les autoriser.",
    );
    expect(
      eventSummary(event({ kind: "escalation", payload: { reason: "x", code: "ai_escalated", detail: "grow the volume", to: "escalated" } })),
    ).toBe("L’IA passe la main\u00a0: grow the volume");
    expect(
      eventSummary(
        event({
          kind: "command_result",
          payload: { exit_code: null, error: "shell commands are disabled on this agent (commands.shell); run `monitorflow-agent.exe ai-commands on`" },
        }),
      ),
    ).toMatch(/^Inachevée\u00a0: Les commandes IA sont désactivées sur cet agent\. Lancez monitorflow-agent\.exe/);
    expect(
      eventSummary(event({ kind: "context_sent", payload: { model: "anthropic/model-1", context: { redactions: { token: 1 } } } })),
    ).toBe("anthropic/model-1, 1 secret masqué");
    expect(timeLeft("2026-10-19T12:00:00Z", new Date("2026-10-19T11:59:15Z"))).toBe("reste 45 s");
    expect(chainSummary({ valid: true, events: 1, broken_at: null, reason: null, head_hash: null })).toBe(
      "Chaîne de hachage vérifiée\u00a0: 1 événement, tête aucune.",
    );
  });

  it("labels actors", () => {
    expect(actorLabel(event({ actor_kind: "user", actor_id: "u1", actor_name: "dave" }))).toBe("dave");
    expect(actorLabel(event({ actor_kind: "user", actor_id: "u1", actor_name: null }))).toBe("deleted user");
    expect(actorLabel(event({ actor_kind: "ai", actor_id: "claude/opus" }))).toBe("AI · claude/opus");
    expect(actorLabel(event({ actor_kind: "peer", actor_id: "agent-web-1" }))).toBe("agent agent-web-1");
  });
});

describe("approval helpers", () => {
  it("counts down to the deadline", () => {
    const deadline = "2026-10-19T12:00:00Z";
    expect(timeLeft(deadline, new Date("2026-10-19T11:59:15Z"))).toBe("45 s left");
    expect(timeLeft(deadline, new Date("2026-10-19T11:39:55Z"))).toBe("20 min 5 s left");
    expect(timeLeft(deadline, new Date("2026-10-19T09:30:00Z"))).toBe("2 h 30 min left");
    expect(timeLeft(deadline, new Date("2026-10-19T12:00:00Z"))).toBeNull();
  });

  it("names tiers, snapshot reasons and chain reports", () => {
    expect(riskWord("high_impact")).toBe("high-impact");
    expect(snapshotReason("not_mapped")).toBe("the server is not mapped to a Proxmox VM.");
    expect(snapshotReason("cannot_snapshot", "lxc")).toBe("the container's storage cannot take snapshots.");
    expect(snapshotReason("cannot_snapshot", "qemu")).toBe("the VM's storage cannot take snapshots.");
    expect(rollbackText({ available: true, reason: null, method: "proxmox", kind: "lxc", left_out: "/shared" })).toEqual({
      text: "A Proxmox snapshot of the container is taken before the command runs.",
      note: "It leaves out /shared: a rollback does not restore it.",
    });
    expect(
      rollbackText({
        available: true,
        reason: "not_mapped",
        method: "lvm",
        detail: "LVM snapshot of / (the thin pool has 40.0 GiB free)",
      }).text,
    ).toBe("A local snapshot is taken before the command runs: LVM snapshot of / (the thin pool has 40.0 GiB free).");
    expect(rollbackText({ available: false, reason: "disabled", detail: "/ is on plain ext4" })).toEqual({
      text: "No snapshot before this change: / is on plain ext4.",
      note: null,
    });
    expect(
      rollbackText({ available: false, reason: "cannot_snapshot", kind: "lxc", detail: "rootfs is on storage local (dir)" }),
    ).toEqual({
      text: "No snapshot before this change: the container's storage cannot take snapshots.",
      note: "(rootfs is on storage local (dir).)",
    });
    expect(chainSummary({ valid: true, events: 3, broken_at: null, reason: null, head_hash: "abcdef0123456789" })).toBe(
      "Hash chain verified: 3 events, head abcdef012345.",
    );
    expect(chainSummary({ valid: false, events: 3, broken_at: 2, reason: "hash", head_hash: null })).toMatch(
      /^Hash chain broken at event 2: an event whose content does not match its hash/,
    );
  });

  it("reads audit filters and drops unknown values", () => {
    const filters = auditFiltersFromQuery(new URLSearchParams("incident=i1&kind=approval&actor=root&server=s1"));
    expect(filters).toEqual({ incident: "i1", server: "s1", kind: "approval", actor: null });
    expect(auditQuery(filters)).toEqual({ incident_id: "i1", server_id: "s1", kind: "approval", actor_kind: undefined });
  });
});
