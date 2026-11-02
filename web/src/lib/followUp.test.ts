import { describe, expect, it } from "vitest";

import type { FollowUp } from "./api";
import { covers, followUpLine, listNames, stepRequest, stepSentence } from "./followUp";

const clock = (iso: string) => iso.slice(11, 16);
const open = { state: "open" as const, severity: "critical" as const, acknowledged_at: null };
const waiting: FollowUp = {
  state: "waiting",
  stop_reason: null,
  step: 0,
  steps: 1,
  started_at: "2026-11-02T03:12:00Z",
  next_at: "2026-11-02T03:27:00Z",
  next: ["bob"],
  policy: "default",
};

describe("follow-up words", () => {
  it("lists names as a sentence does", () => {
    expect(listNames([])).toBe("");
    expect(listNames(["bob"])).toBe("bob");
    expect(listNames(["bob", "Slack #ops"])).toBe("bob and Slack #ops");
    expect(listNames(["alice", "bob", "carol"])).toBe("alice, bob and carol");
  });

  it("says who is told next, and when it stops", () => {
    expect(followUpLine(open, waiting, clock)).toBe("bob will be told at 03:27 if nobody acknowledges.");
    expect(followUpLine(open, { ...waiting, next: [] }, clock)).toMatch(/^Nobody can be told next/);
    expect(followUpLine(open, { ...waiting, state: "silenced" }, clock)).toBe(
      "Silenced: after it ends, bob will be told at 03:27 if nobody acknowledges.",
    );
    const stopped = { ...waiting, state: "stopped" as const, next_at: null, next: [] };
    expect(followUpLine(open, { ...stopped, stop_reason: "finished" }, clock)).toBe("Everyone in the follow-up was told; nobody acknowledged.");
    expect(followUpLine(open, { ...stopped, stop_reason: "not_critical" }, clock)).toBe("No longer critical: nobody else is told.");
    expect(followUpLine(open, { ...stopped, stop_reason: "acknowledged" }, clock)).toBeNull();
    expect(followUpLine({ ...open, severity: "warning" }, null, clock)).toBe("Warnings are not followed up.");
    expect(followUpLine(open, null, clock)).toBeNull();
    expect(followUpLine({ ...open, acknowledged_at: "2026-11-02T03:20:00Z" }, waiting, clock)).toBeNull();
    expect(followUpLine({ ...open, state: "resolved" }, waiting, clock)).toBeNull();
  });

  it("writes a step as a sentence and back as a request", () => {
    const step = {
      after_minutes: 15,
      on_call: [{ id: "s1", name: "Ops" }],
      users: [{ id: "u1", name: "alice" }, { id: "u2", name: null }],
      channels: [{ id: "c1", name: "Slack #ops" }],
      administrators: true,
    };
    expect(stepSentence(step)).toBe(
      "After 15 minutes without acknowledgement, tell the person on call of Ops, alice, a removed user, Slack #ops and the administrators of the place.",
    );
    expect(stepSentence({ ...step, after_minutes: 1, on_call: [], users: [], channels: [], administrators: false })).toBe(
      "After 1 minute without acknowledgement, tell nobody.",
    );
    expect(stepRequest(step)).toEqual({ after_minutes: 15, on_call: ["s1"], users: ["u1", "u2"], channels: ["c1"], administrators: true });
  });

  it("offers only schedules and channels that cover the whole place", () => {
    const sites = new Map([
      ["paris", "acme"],
      ["lyon", "acme"],
    ]);
    const everything = { customer_id: null, site_id: null };
    const acme = { customer_id: "acme", site_id: null };
    const paris = { customer_id: null, site_id: "paris" };
    expect(covers(everything, acme, sites)).toBe(true);
    expect(covers(acme, paris, sites)).toBe(true);
    expect(covers(acme, { customer_id: "globex", site_id: null }, sites)).toBe(false);
    expect(covers(paris, acme, sites)).toBe(false);
    expect(covers(paris, { customer_id: null, site_id: "lyon" }, sites)).toBe(false);
    expect(covers(paris, paris, sites)).toBe(true);
  });
});
