import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { BackupRun, BackupsView } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { BackupsPage, backupSummary } from "./BackupsPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const NAME = "monitorflow-backup-20261029T030000Z-daily.tar.gz";

function view(overrides: Partial<BackupsView> = {}): BackupsView {
  const at = new Date(Date.now() - 3_600_000).toISOString();
  return {
    enabled: true,
    downloads: true,
    updater_alive: true,
    requested: false,
    keep: 7,
    last: run({ finished_at: at }),
    last_success_at: at,
    next_at: new Date(Date.now() + 20 * 3_600_000).toISOString(),
    files: [{ name: NAME, size: 3 * 1024 * 1024, created_at: at, kind: "daily" }],
    ...overrides,
  };
}

function run(overrides: Partial<BackupRun> = {}): BackupRun {
  return {
    id: "d1",
    kind: "daily",
    requested_by: "",
    state: "succeeded",
    error: null,
    warning: null,
    file: NAME,
    started_at: new Date().toISOString(),
    finished_at: new Date().toISOString(),
    ...overrides,
  };
}

function renderPage(user = globalAdmin) {
  return render(
    <As user={user}>
      <BackupsPage />
    </As>,
  );
}

describe("BackupsPage", () => {
  beforeEach(() => setUrl("/admin/backups/"));

  it("is for global administrators only", () => {
    stubFetch({});
    renderPage(customerAdmin);
    expect(screen.getByText("Backups need a global administrator")).toBeInTheDocument();
  });

  it("shows the last backup, warns about secrets, and downloads files", async () => {
    stubFetch({ "GET /api/backups": { status: 200, body: view() } });
    renderPage();
    expect(await screen.findByText("Backed up.")).toBeInTheDocument();
    expect(screen.getByText(/Anyone with the file can read every customer's data/)).toBeInTheDocument();
    expect(screen.getAllByText("3 MiB").length).toBe(2);
    const table = screen.getByRole("table", { name: "Backups" });
    expect(within(table).getByText("Daily")).toBeInTheDocument();
    const download = within(table).getByRole("link", { name: "Download" });
    expect(download).toHaveAttribute("href", `/api/backups/${NAME}`);
    expect(download).toHaveAttribute("download", NAME);
    expect(screen.getByText(`deploy/restore.sh ${NAME}`)).toBeInTheDocument();
  });

  it("backs up now and follows the run", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/backups": [
        { status: 200, body: view() },
        { status: 200, body: view({ requested: true }) },
      ],
      "POST /api/backups": { status: 202, body: { id: "b1" } },
    });
    renderPage();
    await user.click(await screen.findByRole("button", { name: "Back up now" }));
    expect(fetchMock.mock.calls.some(([url, init]) => url === "/api/backups" && init?.method === "POST")).toBe(true);
    expect(await screen.findByRole("button", { name: "Backing up…" })).toBeDisabled();
  });

  it("makes a failure and a missing updater plain", async () => {
    stubFetch({
      "GET /api/backups": {
        status: 200,
        body: view({ updater_alive: false, last: run({ state: "failed", error: "database dump failed: refused", file: null }) }),
      },
    });
    renderPage();
    expect(await screen.findByText("The last backup failed: database dump failed: refused")).toBeInTheDocument();
    expect(screen.getByText(/The updater is not running/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Back up now" })).toBeDisabled();
  });

  it("names the next action when there is no backup yet", async () => {
    stubFetch({ "GET /api/backups": { status: 200, body: view({ last: null, last_success_at: null, files: [] }) } });
    renderPage();
    expect(await screen.findByText("No backup yet")).toBeInTheDocument();
    expect(screen.getByText(/taken a few minutes after the server starts/)).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "Back up now" }).length).toBeGreaterThan(0);
  });
});

describe("backupSummary", () => {
  it("warns when the last good backup is old", () => {
    const now = new Date("2026-10-29T12:00:00Z");
    const old = view({ last_success_at: "2026-10-26T12:00:00Z", last: run() });
    expect(backupSummary(old, now)).toEqual(["The last backup is more than two days old.", "warn"]);
    expect(backupSummary(view({ last_success_at: "2026-10-29T03:00:00Z" }), now)[1]).toBe("ok");
  });
});
