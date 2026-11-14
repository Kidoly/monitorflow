import type { Server, ServerState } from "./api";
import type { Tone } from "@/components/ui/tone";

/** The states in the order of the status strip (ADR-0130). */
export const SERVER_STATES: readonly ServerState[] = ["critical", "warning", "acknowledged", "silenced", "offline", "ok"];

/** The word and tint of each state. OK is the quiet one. */
export const STATE_WORDS: Record<ServerState, readonly [string, Tone]> = {
  critical: ["Critical", "bad"],
  warning: ["Warning", "warn"],
  offline: ["Offline", "bad"],
  acknowledged: ["Acknowledged", "info"],
  silenced: ["Silenced", "neutral"],
  ok: ["OK", "ok"],
};

export function isServerState(value: string | null): value is ServerState {
  return value !== null && (SERVER_STATES as readonly string[]).includes(value);
}

/**
 * The server's state, or for a central server older than it, a guess from
 * the other fields: it cannot tell a warning from a critical one, nor an
 * acknowledged one.
 */
export function serverState(server: Pick<Server, "state" | "status" | "open_incidents" | "silenced_until">): ServerState {
  if (server.state) return server.state;
  if (server.silenced_until) return "silenced";
  if (server.status === "offline") return "offline";
  return server.open_incidents > 0 ? "warning" : "ok";
}

/** Link to the fleet showing the servers in `state`. */
export const stateHref = (state: ServerState) => `/?state=${state}`;
