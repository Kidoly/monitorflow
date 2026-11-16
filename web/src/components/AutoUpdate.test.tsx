import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { AutoUpdatePeer, AutoUpdatePeerList, AutoUpdateStatus } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { AutoUpdatePage, summary } from "./AutoUpdate";

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
  s.agents.history = [s.agents.rollout];
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
    expect(screen.getByText("Déploiement de 0.3.0 : 1 en cours, 4 en attente.")).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "Agents par site" })).toHaveTextContent("3 à jour · 1 en cours · 4 en attente");
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
    expect(await screen.findByText("Rolling out 0.3.0: 1 in progress, 4 waiting.")).toBeInTheDocument();
    const sites = screen.getByRole("table", { name: "Agents by site" });
    const paris = within(sites).getByRole("row", { name: /paris/ });
    expect(paris).toHaveTextContent("server default");
    expect(paris).toHaveTextContent("Rolling out");
    expect(paris).toHaveTextContent("3 up to date · 1 in progress · 4 waiting");
    expect(within(sites).getByRole("row", { name: /lyon/ })).toHaveTextContent("Off");
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
    await user.click(await screen.findByRole("button", { name: "Pause automatic updates" }));
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

function peer(overrides: Partial<AutoUpdatePeer> = {}): AutoUpdatePeer {
  return {
    id: "srv-1",
    name: "web-01",
    site_id: "s1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    version: "0.2.0",
    state: "waiting",
    waiting_for_relay: false,
    error: null,
    finished_at: null,
    ...overrides,
  };
}

const waiting: AutoUpdatePeerList = {
  target: "0.3.0",
  total: 4,
  peers: [
    peer(),
    peer({ id: "srv-2", name: "web-02", waiting_for_relay: true }),
    peer({ id: "srv-3", name: "db-01" }),
    peer({ id: "srv-4", name: "db-02" }),
  ],
};
const AGENTS_WAITING = "GET /api/auto-update/agent/peers?state=waiting&limit=500";

describe("the list behind a tile", () => {
  beforeEach(() => setUrl("/admin/updates/"));

  it("opens under the tiles and closes on a second click", async () => {
    const user = userEvent.setup();
    stubFetch({ "GET /api/auto-update": { status: 200, body: status() }, [AGENTS_WAITING]: { status: 200, body: waiting } });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    const agents = await screen.findByRole("group", { name: "Agents" });
    const tile = within(agents).getByRole("button", { name: /Waiting/ });
    expect(tile).toHaveAttribute("aria-expanded", "false");
    // Nothing to list behind a zero.
    expect(within(agents).queryByRole("button", { name: /Failed/ })).not.toBeInTheDocument();

    await user.click(tile);
    expect(tile).toHaveAttribute("aria-expanded", "true");
    expect(window.location.search).toBe("?agents=waiting");
    const list = await screen.findByRole("table", { name: "Agents waiting" });
    expect(document.getElementById(tile.getAttribute("aria-controls")!)).toContainElement(list);
    expect(within(list).getByRole("link", { name: "web-01" })).toHaveAttribute("href", expect.stringMatching(/^\/servers\/view\/?\?id=srv-1$/));
    expect(within(list).getByRole("row", { name: /web-02/ })).toHaveTextContent("Waits for its relay to be updated first");
    expect(within(list).getByRole("row", { name: /web-01/ })).toHaveTextContent("0.2.0");

    await user.click(tile);
    expect(tile).toHaveAttribute("aria-expanded", "false");
    expect(window.location.search).toBe("");
    expect(screen.queryByRole("table", { name: "Agents waiting" })).not.toBeInTheDocument();
  });

  it("opens from the URL, with why each update failed", async () => {
    setUrl("/admin/updates/?relays=failed");
    const s = status();
    s.relays = { ...s.relays, totals: { ...zero, failed: 1 } };
    const failed = peer({
      id: "relay-1",
      name: "gateway",
      state: "failed",
      error: "the new version did not connect",
      finished_at: "2026-10-27T10:05:00Z",
    });
    stubFetch({
      "GET /api/auto-update": { status: 200, body: s },
      "GET /api/auto-update/relay/peers?state=failed&limit=500": {
        status: 200,
        body: { target: "0.3.0", total: 1, peers: [failed] },
      },
    });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    const list = await screen.findByRole("table", { name: "Relays whose update failed" });
    expect(within(list).getByRole("link", { name: "gateway" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/settings\/relays\/view\/?\?id=relay-1$/),
    );
    expect(within(list).getByRole("row", { name: /gateway/ })).toHaveTextContent("the new version did not connect");
    expect(within(list).getByRole("row", { name: /gateway/ }).querySelector("time")).toHaveAttribute(
      "datetime",
      "2026-10-27T10:05:00.000Z",
    );
    const relays = screen.getByRole("group", { name: "Relays" });
    expect(within(relays).getByRole("button", { name: /Failed/ })).toHaveAttribute("aria-expanded", "true");
  });

  it("opens for one site from its count, and widens to every site", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/auto-update": { status: 200, body: status() },
      [`${AGENTS_WAITING}&site_id=s1`]: { status: 200, body: { ...waiting, total: 5, peers: waiting.peers } },
      [AGENTS_WAITING]: { status: 200, body: waiting },
    });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    const sites = await screen.findByRole("table", { name: "Agents by site" });
    const paris = within(sites).getByRole("row", { name: /paris/ });
    const link = within(paris).getByRole("link", { name: "4 waiting" });
    expect(link).toHaveAttribute("href", "/admin/updates/?agents=waiting&agentsSite=s1");
    // Zeros are plain text.
    expect(within(within(sites).getByRole("row", { name: /lyon/ })).queryByRole("link")).not.toBeInTheDocument();

    await user.click(link);
    expect(window.location.search).toBe("?agents=waiting&agentsSite=s1");
    expect(await screen.findByRole("table", { name: "Agents waiting in paris" })).toBeInTheDocument();
    expect(screen.getByText("The first 4 of 5.")).toBeInTheDocument();
    expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("site_id=s1"))).toBe(true);

    await user.click(screen.getByRole("button", { name: "All sites" }));
    expect(window.location.search).toBe("?agents=waiting");
    expect(await screen.findByRole("table", { name: "Agents waiting" })).toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    setUrl("/admin/updates/?agents=waiting");
    stubFetch({ "GET /api/auto-update": { status: 200, body: status() }, [AGENTS_WAITING]: { status: 200, body: waiting } });
    render(
      <As user={globalAdmin}>
        <AutoUpdatePage />
      </As>,
    );
    const list = await screen.findByRole("table", { name: "Agents en attente" });
    expect(within(list).getByRole("row", { name: /web-02/ })).toHaveTextContent(
      "Attend d’abord la mise à jour de son relais",
    );
    expect(within(list).getByRole("columnheader", { name: "Serveur" })).toBeInTheDocument();
    const paris = within(screen.getByRole("table", { name: "Agents par site" })).getByRole("row", { name: /paris/ });
    expect(within(paris).getByRole("link", { name: "4 en attente" })).toBeInTheDocument();
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
