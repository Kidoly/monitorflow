import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { ServerUpdateCheck, ServerUpdateHistoryEntry, ServerUpdateRun, ServerUpdateView } from "@/lib/api";
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
    milestones: [],
    requested: false,
    available: null,
    ...overrides,
  };
}

const MILESTONES: ServerUpdateView["milestones"] = [
  { kind: "verified", done_at: "2026-10-27T10:00:00Z" },
  { kind: "images_pulled", done_at: null },
  { kind: "backed_up", done_at: "2026-10-27T10:01:00Z" },
  { kind: "database_migrated", done_at: null },
  { kind: "server_restarted", done_at: null },
  { kind: "agents_ready", done_at: null },
];

/** Every page asks for the history; most tests need none. */
const HISTORY = { "GET /api/server-update/history": { status: 200, body: { updates: [] } } };

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
  beforeEach(() => setUrl("/settings/server-update/"));

  it("is for global administrators only", () => {
    stubFetch({
      ...HISTORY,});
    renderPage(customerAdmin);
    expect(screen.getByText("Server updates need a global administrator")).toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    const user = userEvent.setup();
    stubFetch({
      ...HISTORY,
      "GET /api/server-update": { status: 200, body: view({ update: run(), milestones: MILESTONES }) },
      "POST /api/server-update/check": { status: 200, body: available },
      "GET /api/agent-releases": { status: 200, body: { versions: [] } },
    });
    renderPage();
    expect(await screen.findByText("prêt")).toBeInTheDocument();
    expect(screen.getByText("build local")).toBeInTheDocument();
    expect(screen.getByText("Mis à jour")).toBeInTheDocument();
    // The version is in the facts and the heading: no green box repeats it.
    expect(screen.queryByText("Le serveur est en version 0.2.0.")).not.toBeInTheDocument();
    expect(screen.getByText("Programme de mise à jour")).toBeInTheDocument();
    expect(screen.getByText("Journal du programme de mise à jour")).toBeInTheDocument();
    const steps = within(screen.getByRole("list", { name: "Étapes" })).getAllByRole("listitem");
    expect(steps.map((li) => li.textContent?.replace(/[\d:]+$/, ""))).toEqual([
      "✓Version vérifiée (Fait)",
      "○Images téléchargées (Non fait)",
      "✓Sauvegarde créée (Fait)",
      "○Base migrée (Non fait)",
      "○Serveur redémarré (Non fait)",
      "○Agents prêts (Non fait)",
    ]);
    // The check is a button of the page header, not a section of its own.
    expect(screen.queryByRole("heading", { name: "Mises à jour" })).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Rechercher des mises à jour" }));
    expect(await screen.findByText("MonitorFlow 0.2.0 est disponible.")).toBeInTheDocument();
    const changes = within(screen.getByRole("list", { name: "Ce qui change" })).getAllByRole("listitem");
    expect(changes[0]).toHaveTextContent("Serveur : 0.1.0 → 0.2.0");
    expect(screen.getByRole("button", { name: "Mettre à jour vers 0.2.0…" })).toBeInTheDocument();
  });

  it("checks for an update, shows what changes and starts it after confirmation", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...HISTORY,
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
      ...HISTORY,
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
    expect(await screen.findByText(/cannot reach the release source/)).toHaveTextContent("Offline servers");
    expect(screen.queryByText(/dns error/)).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    await user.click(check);
    expect(await screen.findByText(/cannot be used: SHA256SUMS.sig does not verify/)).toBeInTheDocument();
    await user.click(check);
    expect(await screen.findByText("MonitorFlow 0.1.0 is up to date.")).toBeInTheDocument();
  });

  it("does not offer an update without a running updater", async () => {
    const user = userEvent.setup();
    stubFetch({
      ...HISTORY,
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

  it("goes straight to the update when the daily check saw a release", async () => {
    stubFetch({
      ...HISTORY,
      "GET /api/server-update": {
        status: 200,
        body: view({ available: { version: "0.2.0", release_notes_url: null, first_seen_at: "2026-10-27T10:00:00Z" } }),
      },
      "POST /api/server-update/check": { status: 200, body: available },
    });
    renderPage();
    expect(await screen.findByRole("button", { name: "Update to 0.2.0…" })).toBeInTheDocument();
  });

  it("imports the chosen release files and says what is on the server", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...HISTORY,
      "GET /api/server-update": { status: 200, body: view() },
      "GET /api/agent-releases": [
        { status: 200, body: { versions: [], latest: null } },
        { status: 200, body: { versions: ["0.3.0"], latest: "0.3.0" } },
      ],
      "POST /api/agent-releases/import": { status: 200, body: { version: "0.3.0", missing: ["install.sh"] } },
    });
    renderPage();
    expect(await screen.findByText(/It holds no release yet/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "releases page" })).toHaveAttribute(
      "href",
      "https://github.com/Kidoly/monitorflow/releases",
    );
    await user.upload(screen.getByLabelText("Release files"), [new File(["1"], "VERSION"), new File(["2"], "SHA256SUMS")]);
    await user.click(screen.getByRole("button", { name: "Import" }));
    expect(await screen.findByText(/Release 0.3.0 is on this server/)).toHaveTextContent(
      "Without install.sh, Linux servers cannot install from this server.",
    );
    expect(await screen.findByText(/It holds 0.3.0/)).toBeInTheDocument();

    const call = fetchMock.mock.calls.find(([path]) => path === "/api/agent-releases/import")!;
    const form = call[1]!.body as FormData;
    expect(form.getAll("files").map((f) => (f as File).name)).toEqual(["VERSION", "SHA256SUMS"]);
    expect(call[1]!.headers).not.toHaveProperty("Content-Type");
    expect(call[1]!.headers).toHaveProperty("X-MonitorFlow-CSRF", "1");
  });

  it("imports from a URL and shows a refusal as it is", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      ...HISTORY,
      "GET /api/server-update": { status: 200, body: view() },
      "GET /api/agent-releases": { status: 200, body: { versions: [], latest: null } },
      "POST /api/agent-releases/import-url": [
        { status: 400, body: { error: "These files are not a genuine MonitorFlow release." } },
        { status: 200, body: { version: "0.3.0", missing: [] } },
      ],
    });
    renderPage();
    await user.type(await screen.findByLabelText("Release directory URL"), "https://files.lan/mf/");
    await user.click(screen.getByRole("button", { name: "Import from URL" }));
    expect(await screen.findByText("These files are not a genuine MonitorFlow release.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Import from URL" }));
    expect(await screen.findByText(/Release 0.3.0 is on this server/)).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([path]) => path === "/api/agent-releases/import-url")!;
    expect(JSON.parse(call[1]!.body as string)).toEqual({ url: "https://files.lan/mf/" });
  });

  it("says when the server is not set up for updates", async () => {
    stubFetch({
      ...HISTORY, "GET /api/server-update": { status: 200, body: view({ enabled: false, updater: null }) } });
    renderPage();
    expect(await screen.findByText(/not set up for updates from the web interface/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Check for updates" })).not.toBeInTheDocument();
  });

  it("tells the steps in words, with the updater's own log closed at the bottom", async () => {
    stubFetch({
      ...HISTORY,
      "GET /api/server-update": {
        status: 200,
        body: view({ update: run({ state: "rolled_back", error: "the new version did not start" }), milestones: MILESTONES }),
      },
    });
    renderPage();
    const steps = await screen.findByRole("list", { name: "Steps" });
    const items = within(steps).getAllByRole("listitem");
    expect(items.map((li) => li.textContent)).toEqual([
      expect.stringMatching(/^✓Version verified \(Done\)[\d:]+$/),
      "✕Images downloaded (Stopped here)",
      expect.stringMatching(/^✓Backup created \(Done\)[\d:]+$/),
      "○Database migrated (Not done)",
      "○Server restarted (Not done)",
      "○Agents ready (Not done)",
    ]);
    // The raw lines are not in the middle of the page any more.
    expect(steps).not.toHaveTextContent("database and server data saved");
    const log = screen.getByText("Updater log").closest("details")!;
    expect(log).not.toHaveAttribute("open");
    expect(log).toHaveTextContent("database and server data saved to monitorflow-backup-20261027T100100Z-update.tar.gz");
    expect(log.parentElement!.lastElementChild).toBe(log);
  });

  it("lists the server's versions, newest first, with who asked and how each ended", async () => {
    const entry = (overrides: Partial<ServerUpdateHistoryEntry>): ServerUpdateHistoryEntry => ({
      id: "u1",
      from: "0.9.0",
      to: "0.9.1",
      requested_by: "admin",
      started_at: "2026-11-21T21:05:57Z",
      finished_at: "2026-11-21T21:06:56Z",
      result: "succeeded",
      error: null,
      inferred: false,
      ...overrides,
    });
    stubFetch({
      "GET /api/server-update": { status: 200, body: view({ update: run() }) },
      "GET /api/server-update/history": {
        status: 200,
        body: {
          updates: [
            entry({}),
            entry({ id: "u2", from: "0.8.0", to: "0.9.0", result: "rolled_back", error: "the new version did not start" }),
            entry({ id: "u3", from: null, to: "0.8.0", requested_by: null, result: "succeeded", inferred: true }),
          ],
        },
      },
    });
    renderPage();
    const table = await screen.findByRole("table", { name: "Server version history" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent("0.9.0 → 0.9.1");
    expect(rows[0]).toHaveTextContent("admin");
    expect(rows[0]).toHaveTextContent("Updated");
    expect(rows[1]).toHaveTextContent("Rolled back the new version did not start");
    expect(rows[2]).toHaveTextContent("0.8.0");
    expect(rows[2]).toHaveTextContent("Unknown");
    // A deduced result says so.
    expect(within(rows[2]!).getByText(/^Updated/).closest("[tabindex]")).toHaveAccessibleDescription(/deduced from the version/);
  });

  it("follows an update through the restart", async () => {
    stubFetch({
      ...HISTORY,
      "GET /api/server-update": [
        { status: 200, body: view({ update: run({ state: "updating", finished_at: null }), milestones: MILESTONES }) },
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
      const steps = screen.getByRole("list", { name: "Steps" });
      expect(steps).toHaveTextContent("Version verified");
    } finally {
      vi.useRealTimers();
    }
  });

  it("gives the restore command after a rollback that followed migrations", async () => {
    stubFetch({
      ...HISTORY,
      "GET /api/server-update": {
        status: 200,
        body: view({ update: run({ state: "rolled_back", migrated: true, error: "the new version did not start" }) }),
      },
    });
    renderPage();
    expect(await screen.findByText("Rolled back")).toBeInTheDocument();
    expect(screen.getByText(/migrations are forward-only/)).toBeInTheDocument();
    expect(screen.getByText("deploy/restore.sh monitorflow-backup-20261027T100100Z-update.tar.gz")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Backups" })).toHaveAttribute("href", "/settings/backups");
  });

  it("reports a rollback without migrations as complete", async () => {
    stubFetch({
      ...HISTORY,
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
