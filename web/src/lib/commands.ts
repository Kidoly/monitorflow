import type { Command, CommandDetail } from "./api";

/** Package names shown in a title before "and N more". */
const SHOWN_PACKAGES = 3;

const strings = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((v): v is string => typeof v === "string") : [];

/**
 * What a command does, in a few words, and its grey sub-line:
 * "Security updates, no reboot" over "every security update".
 */
export function describeCommand(command: Command): { title: string; sub: string } {
  const p = command.parameters ?? {};
  switch (command.kind) {
    case "apply_updates": {
      const packages = strings(p.packages);
      const reboot = p.allow_reboot === true ? ", allow reboot" : ", no reboot";
      if (packages.length > 0) {
        const names = packages.slice(0, SHOWN_PACKAGES).join(", ");
        const more = packages.length - SHOWN_PACKAGES;
        return {
          title: `Selected: ${names}${more > 0 ? ` and ${more} more` : ""}${p.allow_reboot === true ? reboot : ""}`,
          sub: `${packages.length} ${packages.length === 1 ? "package" : "packages"}`,
        };
      }
      return p.security_only === true
        ? { title: `Security updates${reboot}`, sub: "every security update" }
        : { title: `All packages${reboot}`, sub: "every upgradable package" };
    }
    case "configure_sources": {
      const repositories = Array.isArray(p.repositories) ? p.repositories.length : 0;
      return {
        title: "Configure package sources",
        sub: `${repositories} ${repositories === 1 ? "repository" : "repositories"}${typeof p.mode === "string" ? ` · ${p.mode.toLowerCase()}` : ""}`,
      };
    }
    case "revert_sources":
      return { title: "Revert package sources", sub: "back to the distribution's own" };
    case "self_update": {
      const version = typeof p.version === "string" ? p.version : "?";
      const timeout = typeof p.health_timeout_seconds === "number" && p.health_timeout_seconds > 0 ? p.health_timeout_seconds : 300;
      return {
        title: `Update agent to ${version}`,
        sub: `${p.allow_downgrade === true ? "downgrade allowed · " : ""}rolls back unless it connects within ${timeout} s`,
      };
    }
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
  const steps: [string, string | null][] = [
    ["queued", command.issued_at],
    ["sent", command.sent_at],
    ["running", command.started_at],
  ];
  if (!isActive(command)) steps.push([command.state, command.finished_at]);
  const text = steps
    .filter((step): step is [string, string] => step[1] !== null)
    .map(([word, at]) => `${word} ${clock(new Date(at))}`)
    .join(" → ");
  return command.exit_code === null ? text : `${text} (exit ${command.exit_code})`;
}

/** The text under a command's row: its steps, its error, then its output. */
export function commandLog(command: CommandDetail, clock: (date: Date) => string): string {
  return [commandSteps(command, clock), command.error, command.stdout, command.stderr]
    .filter((part): part is string => Boolean(part))
    .map((part) => part.replace(/\n+$/, ""))
    .join("\n");
}
