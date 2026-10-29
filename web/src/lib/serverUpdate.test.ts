import { describe, expect, it } from "vitest";

import type { ServerUpdateCheck, ServerUpdateRun } from "./api";
import { changeLines, imageText, inProgress, restoreCommand } from "./serverUpdate";

const run = (state: ServerUpdateRun["state"]): ServerUpdateRun => ({
  id: "r1",
  from: "0.1.0",
  to: "0.2.0",
  requested_by: "admin",
  state,
  steps: [],
  error: null,
  backup: null,
  migrated: false,
  started_at: "2026-10-27T10:00:00Z",
  finished_at: null,
});

describe("server update helpers", () => {
  it("knows when an update is under way", () => {
    expect(inProgress(null, false)).toBe(false);
    expect(inProgress(null, true)).toBe(true);
    expect(inProgress(run("backing_up"), false)).toBe(true);
    expect(inProgress(run("rolling_back"), false)).toBe(true);
    expect(inProgress(run("rolled_back"), false)).toBe(false);
  });

  it("shortens images and names local builds", () => {
    expect(imageText(`ghcr.io/kidoly/monitorflow-web@sha256:${"ab".repeat(32)}`)).toBe(
      "monitorflow-web@sha256:abababababab…",
    );
    expect(imageText(null)).toBe("local build");
  });

  it("lists what an update changes", () => {
    const check: ServerUpdateCheck = {
      source: "https://github.com/Kidoly/monitorflow/releases/latest/download",
      running_version: "0.1.0",
      reachable: true,
      error: null,
      latest: {
        version: "0.2.0",
        newer: true,
        release_notes_url: null,
        images: { server: "s", web: "w", updater: "u" },
        web_changes: false,
        updater_changes: true,
        agent_release_cached: true,
      },
    };
    expect(changeLines(check)).toEqual([
      "Server: 0.1.0 → 0.2.0",
      "Web interface: unchanged",
      "Updater: new image, which replaces itself last",
      "Agent release 0.2.0: already cached",
    ]);
    expect(changeLines({ ...check, latest: null })).toEqual([]);
  });

  it("restores the named backup in one command", () => {
    expect(restoreCommand("monitorflow-backup-20261027T100000Z-update.tar.gz")).toBe(
      "deploy/restore.sh monitorflow-backup-20261027T100000Z-update.tar.gz",
    );
  });
});
