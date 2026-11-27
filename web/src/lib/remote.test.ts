import { describe, expect, it } from "vitest";

import type { DesktopFrame } from "./api";
import {
  clock,
  decodeChunk,
  desktopAcceptance,
  desktopSize,
  framesToDraw,
  hasDesktopTab,
  isOpen,
  isWhole,
  parseEvent,
  socketUrl,
  terminalAcceptance,
} from "./remote";

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

describe("remote desktops", () => {
  const access = { can_open: true, can_review: true, recorded: true, idle_timeout_minutes: 15 };

  it("have a tab on Windows servers only, for those who may open or review sessions", () => {
    expect(hasDesktopTab({ kind: "agent", os_family: "windows", remote_terminal: access })).toBe(true);
    expect(hasDesktopTab({ kind: "agent", os_family: "debian", remote_terminal: access })).toBe(false);
    expect(hasDesktopTab({ kind: "device", os_family: "windows", remote_terminal: access })).toBe(false);
    expect(hasDesktopTab({ kind: "agent", os_family: "windows" })).toBe(false);
    const none = { ...access, can_open: false, can_review: false };
    expect(hasDesktopTab({ kind: "agent", os_family: "windows", remote_terminal: none })).toBe(false);
  });

  it("read the agent's own report, apart from terminals", () => {
    expect(desktopAcceptance({ command_kinds: { remote_session: "accepted" } })).toBe("unknown");
    expect(desktopAcceptance({ command_kinds: { remote_desktop: "off" } })).toBe("off");
    expect(terminalAcceptance({ command_kinds: { remote_desktop: "accepted" } })).toBe("unknown");
  });

  it("ask for a 16:9 desktop as wide as the page, within bounds", () => {
    expect(desktopSize(1280)).toEqual({ width: 1280, height: 720 });
    expect(desktopSize(300)).toEqual({ width: 1024, height: 576 });
    expect(desktopSize(5000)).toEqual({ width: 2560, height: 1440 });
    expect(desktopSize(1365)).toEqual({ width: 1366, height: 768 });
  });
});

describe("desktop recordings", () => {
  const frame = (seq: number, at_ms: number, whole: boolean): DesktopFrame => ({
    seq,
    at_ms,
    x: whole ? 0 : 10,
    y: whole ? 0 : 10,
    width: whole ? 800 : 20,
    height: whole ? 600 : 20,
    screen_width: 800,
    screen_height: 600,
    bytes: 100,
  });
  const frames = [frame(0, 0, true), frame(1, 1000, false), frame(2, 2000, false), frame(3, 60000, true), frame(4, 61000, false)];
  const seqs = (r: { draw: DesktopFrame[]; drawn: number }) => [r.draw.map((f) => f.seq), r.drawn];

  it("tells whole pictures apart", () => {
    expect(isWhole(frames[0]!)).toBe(true);
    expect(isWhole(frames[1]!)).toBe(false);
  });

  it("draws forward from what is drawn, and starts again from a whole picture otherwise", () => {
    expect(seqs(framesToDraw(frames, -1, 1500))).toEqual([[0, 1], 1]);
    expect(seqs(framesToDraw(frames, 1, 2500))).toEqual([[2], 2]);
    expect(seqs(framesToDraw(frames, 2, 2500))).toEqual([[], 2]);
    // Past a whole picture: from it, not every picture on the way.
    expect(seqs(framesToDraw(frames, 1, 61500))).toEqual([[3, 4], 4]);
    // Back: from the whole picture before.
    expect(seqs(framesToDraw(frames, 4, 1200))).toEqual([[0, 1], 1]);
    expect(seqs(framesToDraw([], -1, 5000))).toEqual([[], -1]);
  });

  it("shows times as a clock", () => {
    expect(clock(5_400)).toBe("0:05");
    expect(clock(750_000)).toBe("12:30");
    expect(clock(3_723_000)).toBe("1:02:03");
    expect(clock(-5)).toBe("0:00");
  });
});
