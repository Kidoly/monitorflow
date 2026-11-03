import { describe, expect, it } from "vitest";

import { authority, peer, rotation } from "@/test/certificateAuthority";

import {
  authorityNotice,
  authorityStatus,
  expiryWarning,
  machineWord,
  nextStep,
  progressSentence,
  stepExplanation,
} from "./certificateAuthority";

const NOW = new Date("2026-11-03T12:00:00Z");

describe("authorityStatus", () => {
  it("names the change under way, an emergency first", () => {
    expect(authorityStatus(authority({ rotation: rotation() }))).toEqual(["Change under way: step 1 of 2", "info"]);
    expect(authorityStatus(authority({ rotation: rotation({ stage: "switched" }) }))[0]).toBe(
      "Change under way: step 2 of 2",
    );
    expect(authorityStatus(authority({ rotation: rotation({ stage: "switched", emergency: true }) }))).toEqual([
      "Emergency change under way",
      "bad",
    ]);
  });

  it("warns about a near end and a restored retired authority", () => {
    expect(authorityStatus(authority({ expires_soon: true, expires_at: "2027-06-01T12:00:00Z" }))).toEqual([
      "Expires on 2027-06-01",
      "warn",
    ]);
    expect(authorityStatus(authority({ restored_retired_at: "2026-10-20T12:00:00Z" }))[1]).toBe("bad");
    expect(authorityStatus(authority())).toEqual(["Valid until 2036-09-29", "ok"]);
  });
});

describe("expiryWarning", () => {
  it("asks for a change and says when it starts by itself", () => {
    const view = authority({
      expires_soon: true,
      expires_at: "2027-06-01T12:00:00Z",
      auto_change_at: "2026-12-03T12:00:00Z",
    });
    expect(expiryWarning(view, NOW)).toBe(
      "The certificate authority expires on 2027-06-01. Change it now: one click, nothing to do on each server. " +
        "If nobody does, the change starts by itself on 2026-12-03.",
    );
    expect(expiryWarning({ ...view, auto_change_at: "2026-11-01T12:00:00Z" }, NOW)).toMatch(/within the next minutes/);
  });

  it("stays quiet when far from the end or once a change runs", () => {
    expect(expiryWarning(authority(), NOW)).toBeNull();
    expect(expiryWarning(authority({ expires_soon: true, rotation: rotation() }), NOW)).toBeNull();
  });
});

describe("progressSentence", () => {
  it("counts who learned or moved and who has not connected since the step began", () => {
    expect(progressSentence(rotation())).toBe(
      "312 of 315 servers and relays learned the new certificate authority; 2 have not connected since the change started.",
    );
    const switched = rotation({ stage: "switched", switched_at: "2026-11-03T11:00:00Z" });
    expect(progressSentence(switched)).toBe(
      "312 of 315 servers and relays moved to the new certificate authority; 2 have not connected since the switch.",
    );
  });

  it("handles a full list, an all-online wait, a truncated list and an empty fleet", () => {
    expect(progressSentence(rotation({ done: 315 }))).toBe("All 315 servers and relays learned the new certificate authority.");
    expect(progressSentence(rotation({ total: 2, done: 1, waiting: [peer()] }))).toBe(
      "1 of 2 servers and relays learned the new certificate authority; 1 still to go.",
    );
    expect(progressSentence(rotation({ total: 400, done: 300 }))).toMatch(/; 100 still to go\.$/);
    expect(progressSentence(rotation({ total: 0, done: 0, waiting: [] }))).toBe("No server or relay to wait for.");
    expect(progressSentence(rotation({ total: 1, done: 1, waiting: [] }))).toBe(
      "All 1 server or relay learned the new certificate authority.",
    );
  });
});

describe("nextStep", () => {
  it("switches, then stops trusting the old authority, naming who would be cut off", () => {
    const first = nextStep(rotation());
    expect(first.label).toBe("Switch now…");
    expect(first.consequence).toMatch(/The 3 servers and relays not done yet will not be able to connect/);
    const second = nextStep(rotation({ stage: "switched", done: 315 }));
    expect(second.label).toBe("Stop trusting the old authority now…");
    expect(second.consequence).toMatch(/Every server and relay is ready\./);
  });
});

describe("stepExplanation", () => {
  it("explains each step without internal words", () => {
    for (const r of [
      rotation(),
      rotation({ stage: "switched" }),
      rotation({ stage: "switched", emergency: true, distrust_at: "2026-11-04T10:00:00Z" }),
    ]) {
      const text = stepExplanation(r, NOW);
      expect(text).not.toMatch(/\bCA\b|peer|rotat|enrol|compromise/i);
    }
    expect(stepExplanation(rotation({ stage: "switched" }), NOW)).toMatch(/copy a new one from the Add servers page/);
  });
});

describe("authorityNotice", () => {
  it("speaks of an emergency, a restore, machines cut off and a near end, in that order", () => {
    expect(authorityNotice(authority())).toBeNull();
    expect(authorityNotice(authority({ rotation: rotation() }))).toBeNull();
    expect(authorityNotice(authority({ rotation: rotation({ emergency: true }) }))).toMatch(/emergency change/);
    expect(authorityNotice(authority({ restored_retired_at: "2026-10-20T12:00:00Z" }))).toMatch(/restore brought back/);
    expect(authorityNotice(authority({ cut_off: [peer({ cut_off: true })] }))).toBe(
      "1 server or relay needs a new install after a change of certificate authority.",
    );
    expect(authorityNotice(authority({ expires_soon: true, expires_at: "2027-06-01T12:00:00Z" }))).toBe(
      "The certificate authority expires on 2027-06-01.",
    );
  });
});

describe("machineWord", () => {
  it("says Server for an agent", () => {
    expect(machineWord("agent")).toBe("Server");
    expect(machineWord("relay")).toBe("Relay");
  });
});
