import { describe, expect, it } from "vitest";

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

  it("reads states from the URL", () => {
    expect(isServerState("silenced")).toBe(true);
    expect(isServerState("online")).toBe(false);
    expect(isServerState(null)).toBe(false);
    expect(stateHref("critical")).toBe("/?state=critical");
  });
});
