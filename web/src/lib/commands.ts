import { t } from "@/i18n";

import type { Command, CommandDetail } from "./api";

/** Package names shown in a title before "and N more". */
const SHOWN_PACKAGES = 3;

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

/**
 * What a command does, in a few words, and its grey sub-line:
 * "Security updates, no reboot" over "every security update". `note` is
 * what every command of its kind does, for a tooltip.
 */
export function describeCommand(command: Command): { title: string; sub: string; note?: string } {
  const p = command.parameters ?? {};
  switch (command.kind) {
    case "apply_updates": {
      const packages = strings(p.packages);
      const reboot = p.allow_reboot === true ? t("server.commands.allowReboot") : t("server.commands.noReboot");
      if (packages.length > 0) {
        const names = packages.slice(0, SHOWN_PACKAGES).join(", ");
        const more = packages.length - SHOWN_PACKAGES;
        return {
          title: t("server.commands.selected", {
            names,
            more: more > 0 ? t("server.commands.more", { count: more }) : "",
            reboot: p.allow_reboot === true ? reboot : "",
          }),
          sub: t("server.commands.packages", { count: packages.length }),
        };
      }
      return p.security_only === true
        ? { title: t("server.commands.security", { reboot }), sub: t("server.commands.securitySub") }
        : { title: t("server.commands.all", { reboot }), sub: t("server.commands.allSub") };
    }
    case "configure_sources": {
      const repositories = Array.isArray(p.repositories) ? p.repositories.length : 0;
      return {
        title: t("server.commands.configure"),
        sub: `${t("server.commands.repositories", { count: repositories })}${typeof p.mode === "string" ? ` · ${p.mode.toLowerCase()}` : ""}`,
      };
    }
    case "revert_sources":
      return { title: t("server.commands.revert"), sub: t("server.commands.revertSub") };
    case "self_update": {
      const version = typeof p.version === "string" ? p.version : "?";
      const timeout = typeof p.health_timeout_seconds === "number" && p.health_timeout_seconds > 0 ? p.health_timeout_seconds : 300;
      // The same on every update: in a tooltip, not under each row.
      return {
        title: t("server.commands.selfUpdate", { version }),
        sub: p.allow_downgrade === true ? t("server.commands.downgrade") : "",
        note: t("server.commands.rollsBack", { seconds: String(timeout) }),
      };
    }
    case "ai_commands":
      return p.on === true
        ? { title: t("server.commands.aiOn"), sub: t("server.commands.fromWeb") }
        : { title: t("server.commands.aiOff"), sub: t("server.commands.fromWeb") };
    default:
      return { title: command.kind, sub: "" };
  }
}

/** Not finished yet: queued, sent or running. */
export const isActive = (command: Pick<Command, "state">) =>
  command.state === "queued" || command.state === "sent" || command.state === "running";

/** The time of the command's latest step, for the grey text beside its state. */
export function lastStepAt(command: Command): string | null {
  return command.finished_at ?? command.started_at ?? command.sent_at;
}

/** "queued 09:12:00 → sent 09:12:03 → running 09:12:04 → failed 09:12:19 (exit 100)". */
export function commandSteps(command: Command, clock: (date: Date) => string): string {
  const steps: [Command["state"], string | null][] = [
    ["queued", command.issued_at],
    ["sent", command.sent_at],
    ["running", command.started_at],
  ];
  if (!isActive(command)) steps.push([command.state, command.finished_at]);
  const text = steps
    .filter((step): step is [Command["state"], string] => step[1] !== null)
    .map(([state, at]) => `${t(`server.commands.step.${state}`)} ${clock(new Date(at))}`)
    .join(" → ");
  return command.exit_code === null ? text : t("server.commands.exit", { steps: text, code: String(command.exit_code) });
}

/** The text under a command's row: its steps, its error, then its output. */
export function commandLog(command: CommandDetail, clock: (date: Date) => string): string {
  return [commandSteps(command, clock), command.error, command.stdout, command.stderr]
    .filter((part): part is string => Boolean(part))
    .map((part) => part.replace(/\n+$/, ""))
    .join("\n");
}
