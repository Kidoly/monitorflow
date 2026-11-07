import { describe, expect, it } from "vitest";

import type { IntegrationDelivery } from "./api";
import {
  EVENT_GROUPS,
  deliveryResult,
  eventName,
  eventsSummary,
  localToIso,
  stateLabel,
} from "./integrations";

const ALL = EVENT_GROUPS.flatMap((g) => g.events.map((e) => e.event));

function delivery(
  overrides: Partial<IntegrationDelivery> = {},
): IntegrationDelivery {
  return {
    id: 1,
    event_id: "e1",
    event: "incident.opened",
    occurred_at: "2026-11-07T10:00:00Z",
    state: "pending",
    replay: false,
    attempts: 0,
    last_status: null,
    last_error: null,
    last_attempt_at: null,
    next_at: null,
    finished_at: null,
    ...overrides,
  };
}

describe("integrations", () => {
  it("lists the sixteen events the API sends", () => {
    expect(ALL).toHaveLength(16);
    expect(new Set(ALL).size).toBe(16);
  });

  it("names events in plain words", () => {
    expect(eventName("server.offline")).toBe("Servers: Stopped reporting");
    expect(eventName("test")).toBe("Test");
    expect(eventName("plugin.added")).toBe("plugin.added");
  });

  it("sums up what an integration asks for", () => {
    expect(eventsSummary(ALL, ALL)).toBe("All events");
    expect(
      eventsSummary(
        ALL.filter((e) => e !== "incident.acknowledged"),
        ALL,
      ),
    ).toBe("All events but Incidents: Acknowledged");
    expect(
      eventsSummary(
        [
          "incident.opened",
          "incident.escalated",
          "incident.acknowledged",
          "incident.resolved",
        ],
        ALL,
      ),
    ).toBe("Incidents");
    expect(
      eventsSummary(
        [
          "server.added",
          "server.removed",
          "approval.waiting",
          "approval.decided",
        ],
        ALL,
      ),
    ).toBe("Servers: Added, Removed; Approvals");
  });

  it("says the state in one word", () => {
    expect(stateLabel({ state: "on", failing_since: null })).toEqual({
      text: "On",
      tone: "ok",
    });
    expect(
      stateLabel({ state: "on", failing_since: "2026-11-07T10:00:00Z" }).text,
    ).toBe("Failing");
    expect(stateLabel({ state: "paused", failing_since: null }).text).toBe(
      "Paused",
    );
    expect(
      stateLabel({ state: "disabled", failing_since: "2026-11-06T10:00:00Z" }),
    ).toEqual({
      text: "Turned off",
      tone: "bad",
    });
  });

  it("says how a delivery went", () => {
    expect(deliveryResult(delivery())).toBe("Waiting to be sent");
    expect(deliveryResult(delivery({ attempts: 1, last_status: 500 }))).toBe(
      "Tried 1 time (500), trying again",
    );
    expect(deliveryResult(delivery({ attempts: 3 }))).toBe(
      "Tried 3 times, trying again",
    );
    expect(
      deliveryResult(
        delivery({ state: "delivered", attempts: 1, last_status: 204 }),
      ),
    ).toBe("Delivered (204)");
    expect(
      deliveryResult(
        delivery({ state: "failed", attempts: 30, last_status: 503 }),
      ),
    ).toBe("Gave up after 30 tries (503)");
  });

  it("reads a local date and time", () => {
    expect(localToIso("")).toBeNull();
    expect(localToIso("nonsense")).toBeNull();
    expect(localToIso("2026-11-07T10:00")).toBe(
      new Date("2026-11-07T10:00").toISOString(),
    );
  });
});
