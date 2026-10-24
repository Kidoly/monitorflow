import { describe, expect, it } from "vitest";

import { event } from "@/test/remediation";

import {
  actorLabel,
  auditFiltersFromQuery,
  auditQuery,
  chainSummary,
  eventSummary,
  riskWord,
  snapshotReason,
  timeLeft,
} from "./remediation";

describe("eventSummary", () => {
  it("describes each kind from its payload", () => {
    const cases: [Partial<Parameters<typeof eventSummary>[0]>, string][] = [
      [
        { kind: "context_sent", payload: { model: "claude/opus", context: { redactions: { password: 2, token: 1 } } } },
        "Context sent to claude/opus, 3 secrets redacted",
      ],
      [{ kind: "ai_proposal", payload: { diagnosis: "disk full", command: "df -h" } }, "Proposed df -h · disk full"],
      [
        { kind: "ai_proposal", payload: { diagnosis: "fixed", command: null, stop: "resolved", reason: "usage dropped" } },
        "Stopped (considers it fixed): usage dropped",
      ],
      [
        { kind: "classification", payload: { tier: "low_impact", rule: { source: "builtin", id: "low.systemctl" }, reason: "restarts a unit" } },
        "Low-impact by rule low.systemctl: restarts a unit",
      ],
      [{ kind: "approval", actor_kind: "system", payload: { command: "df -h" } }, "Approved by policy: df -h"],
      [{ kind: "approval", actor_kind: "user", payload: { command: "df -h" } }, "Approved: df -h"],
      [{ kind: "edit", payload: { original: "a", edited: "b" } }, "Edited a → b"],
      [{ kind: "rejection", payload: { command: "reboot", reason: "not now" } }, "Rejected reboot · not now"],
      [{ kind: "snapshot", payload: { method: "none", name: null, detail: "rollback is limited" } }, "No snapshot: rollback is limited"],
      [{ kind: "snapshot", payload: { method: "proxmox", name: "mf-1-2", detail: "" } }, "Snapshot mf-1-2 (proxmox)"],
      [{ kind: "command_queued", payload: { command: "df -h" } }, "Queued df -h"],
      [{ kind: "command_result", payload: { exit_code: 0, duration_ms: 1500 } }, "Exit code 0, 1.5 s"],
      [{ kind: "command_result", payload: { exit_code: null, error: "expired", duration_ms: 20 } }, "Did not complete: expired, 20 ms"],
      [{ kind: "escalation", payload: { reason: "limit" } }, "Escalated: limit"],
      [{ kind: "state_change", payload: { from: "awaiting_approval", to: "running" } }, "State awaiting approval → running"],
      [{ kind: "note", payload: { text: "the sensor returned to normal" } }, "the sensor returned to normal"],
    ];
    for (const [overrides, want] of cases) {
      expect(eventSummary(event(overrides))).toBe(want);
    }
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
