import { t } from "@/i18n";

import type { Server } from "./api";

/** What an agent does with one kind of command; `unknown` is neither yes nor no (ADR-0076). */
export type Acceptance = "accepted" | "off" | "unsupported" | "unknown";

/** The kinds the summary names; AI commands (`shell`) have their own line and badge. */
const KINDS = ["apply_updates", "configure_sources", "self_update", "software"] as const;

/** `word` as the API gave it; anything else, or nothing, is unknown. */
export function acceptance(word: string | null | undefined): Acceptance {
  return word === "accepted" || word === "off" || word === "unsupported" ? word : "unknown";
}

type Reporting = Pick<Server, "command_kinds">;

/** What the server's agent last said it does with AI commands. */
export const aiCommands = (server: Reporting): Acceptance => acceptance(server.command_kinds?.shell);

/**
 * What the web interface may do with the server's AI commands (ADR-0098): turn them on and off
 * where its host allowed it, only off elsewhere, nothing with an agent too old to know the switch.
 */
export function aiSwitch(server: Reporting): "both" | "off" | "none" {
  const word = server.command_kinds?.ai_commands;
  return word === "accepted" ? "both" : word === "off" ? "off" : "none";
}

/** "Accepts agent updates. Refuses AI commands.", or null while the agent has not said. */
export function commandSummary(server: Reporting): string | null {
  const kinds = server.command_kinds;
  if (!kinds) return null;
  const named = (state: (a: Acceptance) => boolean) =>
    KINDS.filter((kind) => state(acceptance(kinds[kind]))).map((kind) => t(`server.kinds.${kind}`));
  const accepts = named((a) => a === "accepted");
  const refuses = named((a) => a === "off" || a === "unsupported");
  return [
    accepts.length > 0 && t("server.kinds.accepts", { kinds: accepts.join(", ") }),
    refuses.length > 0 && t("server.kinds.refuses", { kinds: refuses.join(", ") }),
  ]
    .filter(Boolean)
    .join(" ");
}

/**
 * Why a server does not run AI commands although nothing on it can turn them on:
 * on Windows, an agent from before AI commands came to Windows (ADR-0084).
 */
export const unsupportedReason = (osFamily: string) =>
  osFamily === "windows"
    ? t("server.kinds.windowsTooOld")
    : t("server.kinds.notYet");
