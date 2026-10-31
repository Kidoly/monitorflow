// Server update display helpers without React (ADR-0056).

import type { ReleaseImported, ServerUpdateCheck, ServerUpdateRun, ServerUpdateState } from "./api";
import type { Tone } from "@/components/ui/tone";

/** The word and tint of each state. */
export const STATE_WORDS: Record<ServerUpdateState, readonly [string, Tone]> = {
  verifying: ["Verifying the release", "info"],
  downloading: ["Downloading", "info"],
  backing_up: ["Backing up the database", "info"],
  updating: ["Restarting on the new version", "info"],
  checking: ["Checking health", "info"],
  caching_agents: ["Caching the agent release", "info"],
  rolling_back: ["Rolling back", "warn"],
  succeeded: ["Updated", "ok"],
  failed: ["Failed", "bad"],
  rolled_back: ["Rolled back", "bad"],
};

export function isFinal(state: ServerUpdateState): boolean {
  return state === "succeeded" || state === "failed" || state === "rolled_back";
}

/** An update is under way: asked for, or not finished. */
export function inProgress(run: ServerUpdateRun | null, requested: boolean): boolean {
  return requested || (run !== null && !isFinal(run.state));
}

/** The image's short form: `monitorflow-server@sha256:3f2a1b0c…`; null is a local build. */
export function imageText(image: string | null): string {
  if (image === null) return "local build";
  const [name, digest] = image.split("@sha256:");
  const short = name!.split("/").pop();
  return digest ? `${short}@sha256:${digest.slice(0, 12)}…` : image;
}

/** What installing the checked release changes, one line each. */
export function changeLines(check: ServerUpdateCheck): string[] {
  const latest = check.latest;
  if (!latest) return [];
  return [
    `Server: ${check.running_version} → ${latest.version}`,
    latest.web_changes ? "Web interface: new image" : "Web interface: unchanged",
    latest.updater_changes ? "Updater: new image, which replaces itself last" : "Updater: unchanged",
    latest.agent_release_cached
      ? `Agent release ${latest.version}: already cached`
      : `Agent release ${latest.version}: cached during the update, so agents can follow`,
  ];
}

/** The page of releases when `source` is GitHub's, else null. */
export function releasesPage(source: string): string | null {
  const repo = source.replace(/\/+$/, "").match(/^(.*)\/releases\/latest\/download$/);
  return repo ? `${repo[1]}/releases` : null;
}

/** What a partial import cannot do, in plain words. */
export function importNotes({ missing }: ReleaseImported): string[] {
  const lines: string[] = [];
  if (missing.includes("install.sh")) lines.push("Without install.sh, Linux hosts cannot install from this server.");
  if (missing.some((name) => name.endsWith(".msi"))) lines.push("Without the MSI, Windows hosts cannot install from this server.");
  return lines;
}

/** The command that restores `backup` on the server host, after downloading it (ADR-0058). */
export function restoreCommand(backup: string): string {
  return `deploy/restore.sh ${backup}`;
}
