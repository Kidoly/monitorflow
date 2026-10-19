import { describe, expect, it } from "vitest";

import { CERT_WARNING_DAYS, certificateState } from "./certificates";

const NOW = new Date("2026-10-19T12:00:00Z");
const inDays = (days: number) => new Date(NOW.getTime() + days * 86_400_000).toISOString();

describe("certificateState", () => {
  it("warns under fourteen days and after expiry", () => {
    expect(CERT_WARNING_DAYS).toBe(14);
    expect(certificateState(inDays(90), NOW)).toBe("valid");
    expect(certificateState(inDays(14), NOW)).toBe("valid");
    expect(certificateState(inDays(13.9), NOW)).toBe("expiring");
    expect(certificateState(inDays(0.01), NOW)).toBe("expiring");
    expect(certificateState(NOW.toISOString(), NOW)).toBe("expired");
    expect(certificateState(inDays(-3), NOW)).toBe("expired");
  });
});
