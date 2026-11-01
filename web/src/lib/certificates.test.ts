import { describe, expect, it } from "vitest";

import { RENEWAL_LATE_DAYS, certificateProblem, certificateState } from "./certificates";

const NOW = new Date("2026-10-19T12:00:00Z");
const inDays = (days: number) => new Date(NOW.getTime() + days * 86_400_000).toISOString();

describe("certificateState", () => {
  it("is late under two days and failed after expiry", () => {
    expect(RENEWAL_LATE_DAYS).toBe(2);
    expect(certificateState(inDays(90), NOW)).toBe("valid");
    expect(certificateState(inDays(2), NOW)).toBe("valid");
    expect(certificateState(inDays(1.9), NOW)).toBe("late");
    expect(certificateState(inDays(0.01), NOW)).toBe("late");
    expect(certificateState(NOW.toISOString(), NOW)).toBe("failed");
    expect(certificateState(inDays(-3), NOW)).toBe("failed");
  });

  it("stays quiet on a new 7-day certificate and until peers should have renewed", () => {
    expect(certificateState(inDays(7), NOW)).toBe("valid");
    expect(certificateState(inDays(7 / 3), NOW)).toBe("valid");
  });
});

describe("certificateProblem", () => {
  it("says nothing while renewal is on time", () => {
    expect(certificateProblem(inDays(5), NOW)).toBeNull();
  });

  it("says how late renewal is", () => {
    expect(certificateProblem(inDays(1.4), NOW)).toBe("Renewal late, expires in 1 day");
    expect(certificateProblem(inDays(1.6), NOW)).toBe("Renewal late, expires in 2 days");
    expect(certificateProblem(inDays(0.4), NOW)).toBe("Renewal late, expires in under a day");
  });

  it("says when renewal failed", () => {
    expect(certificateProblem(inDays(-0.2), NOW)).toBe("Renewal failed, expired today");
    expect(certificateProblem(inDays(-3), NOW)).toBe("Renewal failed, expired 3 days ago");
  });
});
