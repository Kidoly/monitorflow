import { describe, expect, it } from "vitest";

import type { ServerState } from "./api";
import { isServerState, serverState, stateHref } from "./serverState";

describe("serverState", () => {
  const base = { status: "online" as const, open_incidents: 0, silenced_until: null };

  it("takes the central server's word", () => {
    expect(serverState({ ...base, state: "acknowledged", open_incidents: 2 })).toBe("acknowledged");
  });

  it("guesses for an older central server, a silence first", () => {
    expect(serverState({ ...base, status: "offline", silenced_until: "2026-11-14T18:00:00Z" })).toBe("silenced");
    expect(serverState({ ...base, status: "offline", open_incidents: 1 })).toBe("offline");
    expect(serverState({ ...base, open_incidents: 1 })).toBe("warning");
    expect(serverState(base)).toBe("ok");
  });

  it("guesses for a state this interface has no word for", () => {
    // A newer central server may add states: never show one as a raw key.
    const state = "draining" as ServerState;
    expect(serverState({ ...base, state })).toBe("ok");
    expect(serverState({ ...base, state, status: "offline" })).toBe("offline");
    expect(serverState({ ...base, state, open_incidents: 2 })).toBe("warning");
  });

  it("reads states from the URL", () => {
    expect(isServerState("silenced")).toBe(true);
    expect(isServerState("online")).toBe(false);
    expect(isServerState(null)).toBe(false);
    expect(stateHref("critical")).toBe("/?state=critical");
  });
});
