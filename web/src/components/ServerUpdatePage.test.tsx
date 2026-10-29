import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ServerUpdateCheck, ServerUpdateRun, ServerUpdateView } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { ServerUpdatePage } from "./ServerUpdatePage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const WEB = `ghcr.io/kidoly/monitorflow-web@sha256:${"c".repeat(64)}`;

function view(overrides: Partial<ServerUpdateView> = {}): ServerUpdateView {
  return {
    running_version: "0.1.0",
    enabled: true,
    source: "https://github.com/Kidoly/monitorflow/releases/latest/download",
    updater: {
      version: "0.1.0",
      heartbeat_at: new Date().toISOString(),
      alive: true,
      running: { version: "0.1.0", server_image: null, web_image: WEB, updater_image: null },
    },
    update: null,
    requested: false,
    ...overrides,
  };
}

function run(overrides: Partial<ServerUpdateRun> = {}): ServerUpdateRun {
  return {
    id: "r1",
    from: "0.1.0",
    to: "0.2.0",
    requested_by: "root",
    state: "succeeded",
    steps: [
      { at: "2026-10-27T10:00:00Z", message: "release 0.2.0 verified" },
      { at: "2026-10-27T10:01:00Z", message: "database and server data saved to monitorflow-backup-20261027T100100Z-update.tar.gz" },
    ],
    error: null,
    backup: "monitorflow-backup-20261027T100100Z-update.tar.gz",
    migrated: false,
    started_at: "2026-10-27T10:00:00Z",
    finished_at: "2026-10-27T10:03:00Z",
    ...overrides,
  };
}

const available: ServerUpdateCheck = {
  source: "https://github.com/Kidoly/monitorflow/releases/latest/download",
  running_version: "0.1.0",
  reachable: true,
  error: null,
  latest: {
    version: "0.2.0",
    newer: true,
    release_notes_url: "https://github.com/Kidoly/monitorflow/releases/tag/v0.2.0",
    images: { server: "s", web: "w", updater: "u" },
    web_changes: true,
    updater_changes: false,
    agent_release_cached: false,
  },
};

function renderPage(user = globalAdmin) {
  return render(
    <As user={user}>
      <ServerUpdatePage />
    </As>,
  );
}

describe("ServerUpdatePage", () => {
  beforeEach(() => setUrl("/admin/server-update/"));

  it("is for global administrators only", () => {
    stubFetch({});
    renderPage(customerAdmin);
    expect(screen.getByText("Server updates need a global administrator")).toBeInTheDocument();
  });

  it("checks for an update, shows what changes and starts it after confirmation", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/server-update": [
        { status: 200, body: view() },
        { status: 200, body: view({ requested: true }) },
      ],
      "POST /api/server-update/check": { status: 200, body: available },
      "POST /api/server-update": { status: 202, body: { id: "r1" } },
    });
    renderPage();
    expect(await screen.findByText("ready")).toBeInTheDocument();
    expect(screen.getByText("monitorflow-web@sha256:cccccccccccc…")).toBeInTheDocument();
    expect(screen.getByText("local build")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("MonitorFlow 0.2.0 is available.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Release notes" })).toHaveAttribute("href", available.latest!.release_notes_url);
    const changes = within(screen.getByRole("list", { name: "What changes" })).getAllByRole("listitem");
    expect(changes.map((li) => li.textContent)).toEqual([
      "Server: 0.1.0 → 0.2.0",
      "Web interface: new image",
      "Updater: unchanged",
      "Agent release 0.2.0: cached during the update, so agents can follow",
    ]);

    await user.click(screen.getByRole("button", { name: "Update to 0.2.0…" }));
    const confirm = screen.getByRole("form", { name: "Update the server to 0.2.0?" });
    expect(confirm).toHaveTextContent("The database and the server data are backed up first.");
    await user.click(within(confirm).getByRole("button", { name: "Update to 0.2.0" }));

    const post = fetchMock.mock.calls.find(([url, init]) => url === "/api/server-update" && init?.method === "POST");
    expect(JSON.parse(String(post![1]!.body))).toEqual({ version: "0.2.0" });
    expect(await screen.findByText("Waiting for the updater to start the update…")).toBeInTheDocument();
  });

  it("explains an offline server and a release that does not verify", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/server-update": { status: 200, body: view() },
      "POST /api/server-update/check": [
        { status: 200, body: { ...available, reachable: false, latest: null, error: "https://github.com/…/VERSION: dns error" } },
        { status: 200, body: { ...available, latest: null, error: "SHA256SUMS.sig does not verify" } },
        { status: 200, body: { ...available, latest: { ...available.latest!, version: "0.1.0", newer: false } } },
      ],
    });
    renderPage();
    const check = await screen.findByRole("button", { name: "Check for updates" });
    await user.click(check);
    expect(await screen.findByText(/No update can be checked: .*dns error/)).toHaveTextContent("Offline servers");
    await user.click(check);
    expect(await screen.findByText(/cannot be used: SHA256SUMS.sig does not verify/)).toBeInTheDocument();
    await user.click(check);
    expect(await screen.findByText("MonitorFlow 0.1.0 is up to date.")).toBeInTheDocument();
  });

  it("does not offer an update without a running updater", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/server-update": {
        status: 200,
        body: view({ updater: { ...view().updater!, alive: false, heartbeat_at: "2026-10-27T08:00:00Z" } }),
      },
      "POST /api/server-update/check": { status: 200, body: available },
    });
    renderPage();
    expect(await screen.findByText("not responding")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Check for updates" }));
    expect(await screen.findByText("The updater is not running, so the update cannot start.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Update to/ })).not.toBeInTheDocument();
  });

  it("says when the server is not set up for updates", async () => {
    stubFetch({ "GET /api/server-update": { status: 200, body: view({ enabled: false, updater: null }) } });
    renderPage();
    expect(await screen.findByText(/not set up for updates from the web interface/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Check for updates" })).not.toBeInTheDocument();
  });

  it("follows an update through the restart", async () => {
    stubFetch({
      "GET /api/server-update": [
        { status: 200, body: view({ update: run({ state: "updating", finished_at: null }) }) },
        { status: 502, body: { error: "bad gateway" } },
      ],
    });
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      renderPage();
      expect(await screen.findByText("Update in progress")).toBeInTheDocument();
      expect(screen.getByText("Restarting on the new version")).toBeInTheDocument();
      await vi.advanceTimersByTimeAsync(3_500);
      expect(await screen.findByText(/Reconnecting…/)).toBeInTheDocument();
      // The last status stays on screen.
      expect(screen.getByRole("list", { name: "Steps" })).toHaveTextContent("release 0.2.0 verified");
    } finally {
      vi.useRealTimers();
    }
  });

  it("gives the restore command after a rollback that followed migrations", async () => {
    stubFetch({
      "GET /api/server-update": {
        status: 200,
        body: view({ update: run({ state: "rolled_back", migrated: true, error: "the new version did not start" }) }),
      },
    });
    renderPage();
    expect(await screen.findByText("Rolled back")).toBeInTheDocument();
    expect(screen.getByText(/migrations are forward-only/)).toBeInTheDocument();
    expect(screen.getByText("deploy/restore.sh monitorflow-backup-20261027T100100Z-update.tar.gz")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/admin/backups");
  });

  it("reports a rollback without migrations as complete", async () => {
    stubFetch({
      "GET /api/server-update": {
        status: 200,
        body: view({ update: run({ state: "rolled_back", error: "the new version did not start: unhealthy" }) }),
      },
    });
    renderPage();
    expect(await screen.findByText(/0.1.0 runs again with the same data/)).toBeInTheDocument();
    expect(screen.queryByText(/restore\.sh/)).not.toBeInTheDocument();
  });
});
