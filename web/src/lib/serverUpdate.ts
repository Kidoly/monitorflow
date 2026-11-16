// Server update display helpers without React (ADR-0056).

import type { ReleaseImported, ServerUpdateCheck, ServerUpdateRun, ServerUpdateState } from "./api";
import type { Tone } from "@/components/ui/tone";
import { t } from "@/i18n";

/** The tint of each state. */
export const STATE_TONES: Record<ServerUpdateState, Tone> = {
  verifying: "info",
  downloading: "info",
  backing_up: "info",
  updating: "info",
  checking: "info",
  caching_agents: "info",
  rolling_back: "warn",
  succeeded: "ok",
  failed: "bad",
  rolled_back: "bad",
};

/** The word of a state, in the active language. */
export const stateWord = (state: ServerUpdateState): string => t(`serverUpdate.state.${state}`);

export function isFinal(state: ServerUpdateState): boolean {
  return state === "succeeded" || state === "failed" || state === "rolled_back";
}

/** An update is under way: asked for, or not finished. */
export function inProgress(run: ServerUpdateRun | null, requested: boolean): boolean {
  return requested || (run !== null && !isFinal(run.state));
}

/** The image's short form: `monitorflow-server@sha256:3f2a1b0c…`; null is a local build. */
export function imageText(image: string | null): string {
  if (image === null) return t("serverUpdate.localBuild");
  const [name, digest] = image.split("@sha256:");
  const short = name!.split("/").pop();
  return digest ? `${short}@sha256:${digest.slice(0, 12)}…` : image;
}

/** What installing the checked release changes, one line each. */
export function changeLines(check: ServerUpdateCheck): string[] {
  const latest = check.latest;
  if (!latest) return [];
  return [
    t("serverUpdate.change.server", { from: check.running_version, to: latest.version }),
    latest.web_changes ? t("serverUpdate.change.webNew") : t("serverUpdate.change.webSame"),
    latest.updater_changes ? t("serverUpdate.change.updaterNew") : t("serverUpdate.change.updaterSame"),
    latest.agent_release_cached
      ? t("serverUpdate.change.agentCached", { version: latest.version })
      : t("serverUpdate.change.agentCaching", { version: latest.version }),
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
  if (missing.includes("install.sh")) lines.push(t("serverUpdate.import.noInstallSh"));
  if (missing.some((name) => name.endsWith(".msi"))) lines.push(t("serverUpdate.import.noMsi"));
  return lines;
}

/** The command that restores `backup` on the server host, after downloading it (ADR-0058). */
export function restoreCommand(backup: string): string {
  return `deploy/restore.sh ${backup}`;
}
