import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { AutoUpdateStatus } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { AutoUpdatePage, summary, versionsText } from "./AutoUpdate";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const zero = { up_to_date: 0, waiting: 0, waiting_for_relay: 0, pending: 0, succeeded: 0, failed: 0 };
const noRelays = { latest: "0.3.0", target: "0.3.0", rollout: null, totals: zero, sites: [], history: [] };

function status(overrides: Partial<AutoUpdateStatus> = {}): AutoUpdateStatus {
  return {
    settings: { enabled: true, enabled_locked: false, paused: false, failure_threshold: 2 },
    server_version: "0.3.0",
    agents: {
      latest: "0.3.0",
      target: "0.3.0",
      rollout: {
        kind: "agent",
        version: "0.3.0",
        state: "rolling",
        started_at: "2026-10-27T10:00:00Z",
        halted_at: null,
        reason: null,
        counted_from: "2026-10-27T10:00:00Z",
      },
      totals: { ...zero, up_to_date: 3, waiting: 4, pending: 1, succeeded: 1 },
      sites: [
        {
          id: "s1",
          name: "paris",
          customer_name: "acme",
          auto_update: null,
          enabled: true,
          phase: "rolling",
          totals: { ...zero, up_to_date: 3, waiting: 4, pending: 1, succeeded: 1 },
        },
        { id: "s2", name: "lyon", customer_name: "acme", auto_update: false, enabled: false, phase: null, totals: zero },
      ],
      history: [],
    },
    relays: noRelays,
    ...overrides,
  };
}

const halted = (): AutoUpdateStatus => {
  const s = status();
  s.agents.rollout = { ...s.agents.rollout!, state: "halted", halted_at: "2026-10-27T11:00:00Z", reason: "2 automatic updates to 0.3.0 failed (threshold 2)" };
  s.agents.history = [{ ...s.agents.rollout, outcome: "halted" }];
  s.agents.sites[0]!.phase = "blocked";
  return s;
};

describe("AutoUpdatePage", () => {
  beforeEach(() => setUrl("/admin/updates/"));

  it("speaks French when that is the language", async () => {
    setActiveLocale("fr");
    stubFetch({ "GET /api/auto-update": { status: 200, body: status() } });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    expect(await screen.findByRole("heading", { name: "Mises à jour automatiques" })).toBeInTheDocument();
    expect(screen.getByText("Cible 0.3.0 : la dernière version détenue, et celle du serveur.")).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "Agents par site" })).toHaveTextContent("En cours");
    expect(screen.getByRole("button", { name: "Suspendre tous les déploiements" })).toBeInTheDocument();
    expect(summary(halted(), halted().agents)[0]).toBe("Interrompu\u00a0: 2 automatic updates to 0.3.0 failed (threshold 2).");
  });

  it("is for global administrators only", () => {
    render(
      <As user={customerAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    expect(screen.getByText("Automatic updates need a global administrator")).toBeInTheDocument();
  });

  it("shows the rollout's progress per site", async () => {
    stubFetch({ "GET /api/auto-update": { status: 200, body: status() } });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    const sites = await screen.findByRole("table", { name: "Agents by site" });
    // The tiles carry the counts; no banner or row repeats them.
    expect(screen.queryByText(/Rolling out 0.3.0/)).not.toBeInTheDocument();
    expect(screen.getByRole("group", { name: "Agents" })).toHaveTextContent("Waiting4");
    const paris = within(sites).getByRole("row", { name: /paris/ });
    expect(paris).toHaveTextContent("In progress");
    expect(paris).not.toHaveTextContent("up to date");
    // Only the site that overrides the server-wide setting says so.
    expect(paris).not.toHaveTextContent(/On|server default/);
    expect(within(sites).getByRole("row", { name: /lyon/ })).toHaveTextContent("Off");
    // The versions are told once for both kinds.
    expect(screen.getAllByText(/Target 0.3.0/)).toHaveLength(1);
    expect(screen.queryByText(/Relays update first/)).not.toBeInTheDocument();
  });

  it("pauses automatic updates and resumes a halted rollout", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/auto-update": [{ status: 200, body: halted() }, { status: 200, body: status() }],
      "PUT /api/settings/auto-update": { status: 200, body: status().settings },
      "POST /api/auto-update/rollouts/agent/0.3.0/resume": { status: 200, body: status().agents.rollout },
    });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("Halted: 2 automatic updates to 0.3.0 failed (threshold 2).");
    expect(within(screen.getByRole("table", { name: "Agents by site" })).getByText("Canary failed")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Resume 0.3.0" }));
    expect(fetchMock.mock.calls.some(([url, init]) => String(url).endsWith("/resume") && init?.method === "POST")).toBe(true);
    await user.click(await screen.findByRole("button", { name: "Pause every rollout" }));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(put?.[1]?.body).toBe(JSON.stringify({ paused: true }));
  });

  it("locks the switch the configuration sets and checks the threshold", async () => {
    const user = userEvent.setup();
    const locked = status();
    locked.settings.enabled_locked = true;
    const fetchMock = stubFetch({
      "GET /api/auto-update": { status: 200, body: locked },
      "PUT /api/settings/auto-update": { status: 200, body: locked.settings },
    });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    expect(await screen.findByText(/set in the server's configuration/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Turn off" })).not.toBeInTheDocument();
    const field = screen.getByLabelText("Halt a rollout after this many failed updates");
    await user.clear(field);
    await user.type(field, "0");
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    await user.clear(field);
    await user.type(field, "5");
    await user.click(screen.getByRole("button", { name: "Save" }));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(put?.[1]?.body).toBe(JSON.stringify({ failure_threshold: 5 }));
  });
});

describe("history", () => {
  beforeEach(() => setUrl("/admin/updates/"));

  it("shows older rollouts as superseded and the target's as finished", async () => {
    const s = status();
    s.agents.totals = { ...zero, up_to_date: 5 };
    const base = s.agents.rollout!;
    s.agents.history = [
      { ...base, outcome: "finished" },
      { ...base, version: "0.2.0", outcome: "superseded" },
    ];
    stubFetch({ "GET /api/auto-update": { status: 200, body: s } });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    const history = await screen.findByRole("table", { name: "Agents rollouts" });
    expect(within(history).getByRole("row", { name: /0\.3\.0/ })).toHaveTextContent("Finished");
    expect(within(history).getByRole("row", { name: /0\.2\.0/ })).toHaveTextContent("Superseded");
    expect(within(history).queryByText("In progress")).not.toBeInTheDocument();
  });

  it("names three different versions when they differ", () => {
    const s = status({ server_version: "0.4.0" });
    s.agents.latest = "0.5.0";
    expect(versionsText(s)).toBe("Target 0.3.0 · newest release held 0.5.0 · server 0.4.0");
  });
});

describe("relays", () => {
  beforeEach(() => setUrl("/admin/updates/"));

  it("come first, and agents behind an older relay wait", async () => {
    const s = status();
    s.agents.totals = { ...s.agents.totals, waiting_for_relay: 2 };
    s.relays = {
      ...s.relays,
      totals: { ...zero, pending: 1 },
      sites: [{ ...s.agents.sites[0]!, phase: "canary", totals: { ...zero, pending: 1 } }],
    };
    stubFetch({ "GET /api/auto-update": { status: 200, body: s } });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    expect(await screen.findByText("2 waiting for their relay to be updated first.")).toBeInTheDocument();
    const relays = screen.getByRole("table", { name: "Relays by site" });
    expect(within(relays).getByRole("row", { name: /paris/ })).toHaveTextContent("Canary");
  });
});

describe("summary", () => {
  it("says why nothing moves", () => {
    const paused = status();
    paused.settings.paused = true;
    expect(summary(paused, paused.agents)[1]).toBe("warn");
    const behind = status();
    behind.agents.target = null;
    behind.agents.latest = "0.4.0";
    expect(summary(behind, behind.agents)[0]).toContain("update the server first");
    const done = status();
    done.agents.totals = { ...zero, up_to_date: 5 };
    expect(summary(done, done.agents)).toEqual([
      "Up to date: every agent where automatic updates apply runs 0.3.0 or newer.",
      "ok",
    ]);
  });
});
