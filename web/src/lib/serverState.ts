import type { Server, ServerState } from "./api";
import type { Tone } from "@/components/ui/tone";
import { t } from "@/i18n";

/** The states in the order of the status strip (ADR-0130). */
export const SERVER_STATES: readonly ServerState[] = ["critical", "warning", "acknowledged", "silenced", "offline", "ok", "waiting"];

/** The tint of each state. OK is the quiet one. */
export const STATE_TONES: Record<ServerState, Tone> = {
  critical: "bad",
  warning: "warn",
  offline: "bad",
  acknowledged: "info",
  silenced: "neutral",
  ok: "ok",
  waiting: "neutral",
};

/** The word of a state, in the active language. */
export const stateWord = (state: ServerState): string => t(`common.state.${state}`);

export function isServerState(value: string | null): value is ServerState {
  return value !== null && (SERVER_STATES as readonly string[]).includes(value);
}

/**
 * The server's state, or for a central server older than it, a guess from
 * the other fields: it cannot tell a warning from a critical one, nor an
 * acknowledged one. A state this interface has no word for, from a newer
 * central server, is guessed the same way: shown as is, it would be a raw
 * catalog key, as `common.state.waiting` was before this page knew it.
 */
export function serverState(server: Pick<Server, "state" | "status" | "open_incidents" | "silenced_until">): ServerState {
  const { state } = server;
  if (state && isServerState(state)) return state;
  if (server.silenced_until) return "silenced";
  if (server.status === "offline") return "offline";
  return server.open_incidents > 0 ? "warning" : "ok";
}

/** Link to the fleet showing the servers in `state`. */
export const stateHref = (state: ServerState) => `/?state=${state}`;
