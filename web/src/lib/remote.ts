import type { DesktopFrame, RemoteSession, ServerDetail } from "./api";
import { acceptance, type Acceptance } from "./commandKinds";

/** What the agent does with remote terminals, as it last reported (ADR-0190). */
export const terminalAcceptance = (server: Pick<ServerDetail, "command_kinds">): Acceptance =>
  acceptance(server.command_kinds?.remote_session);

/** What the agent does with remote desktops, as it last reported (ADR-0190). */
export const desktopAcceptance = (server: Pick<ServerDetail, "command_kinds">): Acceptance =>
  acceptance(server.command_kinds?.remote_desktop);

/** Whether the server's page has a Remote desktop tab: Windows servers, for those who may open sessions or see them. */
export const hasDesktopTab = (server: Pick<ServerDetail, "os_family" | "remote_terminal" | "kind">): boolean =>
  server.kind !== "device" &&
  server.os_family === "windows" &&
  !!server.remote_terminal &&
  (server.remote_terminal.can_open || server.remote_terminal.can_review);

/** A size for a new desktop in a box `width` pixels wide: 16:9, from 1024×576 to 2560×1440, even numbers. */
export function desktopSize(width: number): { width: number; height: number } {
  const w = Math.min(2560, Math.max(1024, Math.round(width / 2) * 2));
  return { width: w, height: Math.round((w * 9) / 16 / 2) * 2 };
}

/** The session's WebSocket on this page's host: `wss://` behind HTTPS. */
export function socketUrl(location: Pick<Location, "protocol" | "host">, sessionId: string): string {
  const scheme = location.protocol === "https:" ? "wss" : "ws";
  return `${scheme}://${location.host}/api/remote-sessions/${encodeURIComponent(sessionId)}/socket`;
}

/** A text message from the session's WebSocket. */
export type SocketEvent = { type: "waiting" } | { type: "opened" } | { type: "closed"; reason: string };

/** `text` as a session event, or null for anything else. */
export function parseEvent(text: string): SocketEvent | null {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return null;
  }
  if (typeof value !== "object" || value === null) return null;
  const { type, reason } = value as { type?: unknown; reason?: unknown };
  if (type === "waiting" || type === "opened") return { type };
  if (type === "closed") return { type, reason: typeof reason === "string" ? reason : "" };
  return null;
}

/** Whether the session is still open. */
export const isOpen = (session: Pick<RemoteSession, "closed_at">) => session.closed_at === null;

/** The bytes of a recording chunk's base64 `data`. */
export function decodeChunk(data: string): Uint8Array {
  const text = atob(data);
  const bytes = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) bytes[i] = text.charCodeAt(i);
  return bytes;
}

/** Whether a picture of a desktop's recording covers the whole screen: a replay can start from it (ADR-0192). */
export const isWhole = (frame: DesktopFrame): boolean =>
  frame.x === 0 && frame.y === 0 && frame.width === frame.screen_width && frame.height === frame.screen_height;

/**
 * The pictures to draw, in order, to show the screen at `ms` when those up to index `drawn` are drawn (-1
 * for none), and the index of the last of them. Going back, or past a whole picture, starts again from
 * the last whole picture before `ms`.
 */
export function framesToDraw(frames: DesktopFrame[], drawn: number, ms: number): { draw: DesktopFrame[]; drawn: number } {
  let target = -1;
  let whole = -1;
  for (const [i, frame] of frames.entries()) {
    if (frame.at_ms > ms) break;
    target = i;
    if (isWhole(frame)) whole = i;
  }
  if (target < 0 || target === drawn) return { draw: [], drawn: target };
  const start = target > drawn && whole <= drawn ? drawn + 1 : Math.max(whole, 0);
  return { draw: frames.slice(start, target + 1), drawn: target };
}

/** "0:05", "12:30", "1:02:03". */
export function clock(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const [h, m, s] = [Math.floor(total / 3600), Math.floor(total / 60) % 60, total % 60];
  const pad = (n: number) => String(n).padStart(2, "0");
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`;
}
