import { describe, expect, it } from "vitest";

import { decodeChunk, isOpen, parseEvent, socketUrl, terminalAcceptance } from "./remote";

describe("remote terminals", () => {
  it("opens the WebSocket on this page's host, secure behind HTTPS", () => {
    expect(socketUrl({ protocol: "https:", host: "mf.example:8443" }, "abc")).toBe(
      "wss://mf.example:8443/api/remote-sessions/abc/socket",
    );
    expect(socketUrl({ protocol: "http:", host: "localhost:3000" }, "a/b")).toBe(
      "ws://localhost:3000/api/remote-sessions/a%2Fb/socket",
    );
  });

  it("reads the session's events and nothing else", () => {
    expect(parseEvent('{"type":"waiting"}')).toEqual({ type: "waiting" });
    expect(parseEvent('{"type":"opened"}')).toEqual({ type: "opened" });
    expect(parseEvent('{"type":"closed","reason":"closed by alice"}')).toEqual({
      type: "closed",
      reason: "closed by alice",
    });
    expect(parseEvent('{"type":"closed"}')).toEqual({ type: "closed", reason: "" });
    for (const other of ["", "null", "[]", '{"type":"input"}', "not json"]) expect(parseEvent(other)).toBeNull();
  });

  it("knows what the agent reported and whether a session is open", () => {
    expect(terminalAcceptance({ command_kinds: { remote_session: "accepted" } })).toBe("accepted");
    expect(terminalAcceptance({ command_kinds: { remote_session: "off" } })).toBe("off");
    expect(terminalAcceptance({ command_kinds: {} })).toBe("unknown");
    expect(terminalAcceptance({ command_kinds: null })).toBe("unknown");
    expect(isOpen({ closed_at: null })).toBe(true);
    expect(isOpen({ closed_at: "2026-11-23T10:00:00Z" })).toBe(false);
  });

  it("decodes recording chunks to bytes", () => {
    expect(Array.from(decodeChunk("aGkNCg=="))).toEqual([104, 105, 13, 10]);
  });
});
