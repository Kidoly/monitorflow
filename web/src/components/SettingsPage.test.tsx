import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { RetentionSettings, ServerSettings } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { SettingsPage, retentionDays } from "./SettingsPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });

function retention(overrides: Partial<RetentionSettings> = {}): RetentionSettings {
  return {
    metrics: { value: "90d", locked: false },
    logs: { value: "30d", locked: false },
    default_metrics: "90d",
    default_logs: "30d",
    max_days: 3650,
    updater_alive: true,
    applying: false,
    error: null,
    ...overrides,
  };
}

function settings(overrides: Partial<ServerSettings> = {}): ServerSettings {
  return {
    session_lifetime: { value: 43200, default: 43200, locked: false },
    session_idle_timeout: { value: 1800, default: 1800, locked: false },
    mirror_public_url: { value: "http://mf.example:7480", default: "http://mf.example:7480", locked: false },
    retention: retention(),
    ...overrides,
  };
}

const address = {
  address: "mf.example",
  default: "mf.example",
  names: ["mf.example"],
  certified: true,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
};
const install = {
  source: "github",
  custom_url: null,
  source_from_config: false,
  custom_url_from_config: false,
  github_url: "https://github.com/x",
  central: null,
};

function stub(view: ServerSettings, more: Parameters<typeof stubFetch>[0] = {}) {
  return stubFetch({
    "GET /api/settings/server": { status: 200, body: view },
    "GET /api/settings/public-address": { status: 200, body: address },
    "GET /api/settings/agent-install": { status: 200, body: install },
    ...more,
  });
}

function open() {
  return render(
    <As user={globalAdmin}>
      <SettingsPage />
    </As>,
  );
}

function sent(fetchMock: ReturnType<typeof stub>, method: string, path: string) {
  const call = fetchMock.mock.calls.find(([url, init]) => String(url) === path && init?.method === method);
  return JSON.parse(String(call?.[1]?.body));
}

describe("SettingsPage", () => {
  beforeEach(() => setUrl("/admin/settings/"));
  afterEach(() => vi.unstubAllGlobals());

  it("is for global administrators", () => {
    render(
      <As user={customerAdmin}>
        <SettingsPage />
      </As>,
    );
    expect(screen.getByText("Settings need a global administrator")).toBeInTheDocument();
  });

  it("gathers the other settings pages and shows the defaults", async () => {
    stub(settings());
    open();
    expect(await screen.findByLabelText(/Sign out after/)).toHaveValue(12);
    expect(screen.getByText(/default 12 hours/)).toBeInTheDocument();
    expect(screen.getByLabelText(/Sign out when idle/)).toHaveValue(30);
    expect(screen.getByLabelText(/Keep metrics/)).toHaveValue(90);
    expect(screen.getByLabelText(/Agents reach this server at/)).toBeInTheDocument();
    for (const label of ["AI", "Notifications", "Automatic updates", "Backups", "Server update", "Users and roles"]) {
      expect(screen.getByRole("link", { name: label })).toBeInTheDocument();
    }
  });

  it("saves sign-in settings in seconds, keeping the others", async () => {
    const fetchMock = stub(settings(), { "PUT /api/settings/server": { status: 200, body: settings() } });
    open();
    const hours = await screen.findByLabelText(/Sign out after/);
    await userEvent.clear(hours);
    await userEvent.type(hours, "8");
    await userEvent.click(within(screen.getByRole("region", { name: "Sign-in" })).getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(sent(fetchMock, "PUT", "/api/settings/server")).toEqual({
      session_lifetime: 28800,
      session_idle_timeout: 1800,
      mirror_public_url: "http://mf.example:7480",
    });
  });

  it("shows a locked value and why, and sends none for it", async () => {
    const view = settings({
      session_lifetime: { value: 3600, default: 43200, locked: true },
      mirror_public_url: { value: "http://config.example", default: null, locked: true },
    });
    const fetchMock = stub(view, { "PUT /api/settings/server": { status: 200, body: view } });
    open();
    await screen.findByLabelText(/Sign out when idle/);
    expect(screen.getByText("SESSION_LIFETIME")).toBeInTheDocument();
    expect(screen.getByText("MIRROR_PUBLIC_URL")).toBeInTheDocument();
    expect(screen.getByText("1 hour")).toBeInTheDocument();
    await userEvent.click(within(screen.getByRole("region", { name: "Sign-in" })).getByRole("button", { name: "Save" }));
    await screen.findByText("Saved.");
    expect(sent(fetchMock, "PUT", "/api/settings/server")).toEqual({
      session_lifetime: null,
      session_idle_timeout: 1800,
      mirror_public_url: null,
    });
  });

  it("refuses a mirror address that cannot work", async () => {
    stub(settings());
    open();
    const url = await screen.findByLabelText(/^Address/);
    await userEvent.clear(url);
    await userEvent.type(url, "ftp://x");
    const mirror = within(screen.getByRole("region", { name: "Package address" }));
    expect(mirror.getByRole("button", { name: "Save" })).toBeDisabled();
    await userEvent.clear(url);
    expect(mirror.getByRole("button", { name: "Save" })).toBeEnabled();
  });

  it("asks before retention gets shorter, and only sends what changed", async () => {
    const fetchMock = stub(settings(), {
      "PUT /api/settings/retention": { status: 202, body: settings() },
    });
    open();
    const metrics = await screen.findByLabelText(/Keep metrics/);
    await userEvent.clear(metrics);
    await userEvent.type(metrics, "60");
    await userEvent.click(screen.getByRole("button", { name: "Save…" }));
    expect(screen.getByText("Delete older data?")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Save and delete older data" }));
    await waitFor(() =>
      expect(sent(fetchMock, "PUT", "/api/settings/retention")).toEqual({ metrics_days: 60, logs_days: null }),
    );
  });

  it("saves a longer retention at once", async () => {
    const fetchMock = stub(settings(), {
      "PUT /api/settings/retention": { status: 202, body: settings() },
    });
    open();
    const logs = await screen.findByLabelText(/Keep logs/);
    await userEvent.clear(logs);
    await userEvent.type(logs, "45");
    await userEvent.click(within(screen.getByRole("region", { name: "Data retention" })).getByRole("button", { name: "Save" }));
    await waitFor(() =>
      expect(sent(fetchMock, "PUT", "/api/settings/retention")).toEqual({ metrics_days: null, logs_days: 45 }),
    );
  });

  it("says what keeps retention from changing", async () => {
    stub(
      settings({
        retention: retention({
          logs: { value: "1y", locked: true },
          updater_alive: false,
          error: "the stores did not start",
        }),
      }),
    );
    open();
    expect(await screen.findByText(/The updater is not running/)).toBeInTheDocument();
    expect(screen.getByText(/the stores did not start/)).toBeInTheDocument();
    expect(screen.getByText("LOGS_RETENTION")).toBeInTheDocument();
    expect(screen.getByText("1y")).toBeInTheDocument();
    expect(screen.getByLabelText(/Keep metrics/)).toBeEnabled();
    const section = within(screen.getByRole("region", { name: "Data retention" }));
    expect(section.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("shows a change being applied, and servers without an updater", async () => {
    stub(settings({ retention: retention({ applying: true }) }));
    const { unmount } = open();
    expect(await screen.findByText(/stores restart for a few seconds/)).toBeInTheDocument();
    expect(screen.getByLabelText(/Keep metrics/)).toBeDisabled();
    unmount();
    stub(settings({ retention: null }));
    open();
    expect(await screen.findByText(/no updater to apply it/)).toBeInTheDocument();
  });
});

describe("retentionDays", () => {
  it("reads days only", () => {
    expect(retentionDays("90d")).toBe(90);
    expect(retentionDays("1y")).toBeNull();
  });
});
