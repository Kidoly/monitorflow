import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { RemoteSession, ServerDetail } from "@/lib/api";
import { As, me, siteAdmin } from "@/test/admin";
import { server } from "@/test/campaigns";
import { stubFetch } from "@/test/fetch";

import { ServerTerminalTab, sessionLength } from "./ServerTerminalTab";

vi.mock("next/navigation", () => import("@/test/navigation"));
// xterm.js needs a real browser: the live terminal is a stand-in here.
vi.mock("next/dynamic", () => ({
  default: () =>
    function Stub({ serverId, sessionId }: { serverId?: string; sessionId?: string }) {
      return <div data-testid="xterm">{serverId ?? sessionId}</div>;
    },
}));

const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });

function detail(overrides: Partial<ServerDetail> = {}): ServerDetail {
  return {
    ...server({ command_kinds: { remote_session: "accepted" } }),
    certificate: null,
    remote_terminal: { can_open: true, can_review: true, recorded: true, idle_timeout_minutes: 15 },
    ...overrides,
  };
}

const session = (overrides: Partial<RemoteSession> = {}): RemoteSession => ({
  id: "s1",
  server_id: "web",
  hostname: "web-1",
  kind: "terminal",
  requested_by: "bob",
  created_at: "2026-11-23T09:30:00Z",
  opened_at: "2026-11-23T09:30:02Z",
  closed_at: "2026-11-23T09:41:15Z",
  close_reason: "closed by bob",
  duration_seconds: 673,
  recorded: true,
  bytes_in: 10,
  bytes_out: 100,
  ...overrides,
});

beforeEach(() => {
  setActiveLocale("en");
  vi.unstubAllGlobals();
});

describe("ServerTerminalTab", () => {
  it("opens a terminal and lists past sessions with their recordings for administrators", async () => {
    const s = detail();
    stubFetch({ [`GET /api/servers/${s.id}/remote-sessions`]: { status: 200, body: [session()] } });
    render(
      <As user={siteAdmin}>
        <ServerTerminalTab server={s} />
      </As>,
    );
    expect(screen.getByText(/closes after 15 minutes without input/)).toBeInTheDocument();
    expect(screen.getByText(/is recorded/)).toBeInTheDocument();
    const table = await screen.findByRole("table", { name: "Remote sessions" });
    expect(within(table).getByText("closed by bob")).toBeInTheDocument();
    expect(within(table).getByText("11 min")).toBeInTheDocument();
    expect(within(table).getByRole("button", { name: "Replay" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Open a terminal" }));
    expect(screen.getByTestId("xterm")).toHaveTextContent(s.id);
    expect(screen.getByRole("button", { name: "Close the session" })).toBeInTheDocument();
  });

  it("lets who opened a session close it, and keeps recordings for administrators", async () => {
    const s = detail();
    const fetchMock = stubFetch({
      [`GET /api/servers/${s.id}/remote-sessions`]: {
        status: 200,
        body: [session({ requested_by: "alice", closed_at: null, close_reason: null })],
      },
      "POST /api/remote-sessions/s1/close": { status: 204 },
    });
    render(
      <As user={operator}>
        <ServerTerminalTab server={s} />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Remote sessions" });
    expect(within(table).getByText("Open now")).toBeInTheDocument();
    expect(within(table).queryByRole("button", { name: "Replay" })).not.toBeInTheDocument();
    await userEvent.click(within(table).getByRole("button", { name: "Close" }));
    expect(fetchMock.mock.calls.some(([url, init]) => url === "/api/remote-sessions/s1/close" && init?.method === "POST")).toBe(true);
  });

  it("gives administrators the line that allows terminals where the server does not", async () => {
    const s = detail({ ...server({ os_family: "windows", command_kinds: { remote_session: "off" } }) });
    stubFetch({ [`GET /api/servers/${s.id}/remote-sessions`]: { status: 200, body: [] } });
    render(
      <As user={siteAdmin}>
        <ServerTerminalTab server={s} />
      </As>,
    );
    expect(screen.getByText(/does not allow remote terminals/)).toBeInTheDocument();
    expect(screen.getByLabelText("Line that allows remote terminals")).toHaveTextContent("remote-access terminal on");
    expect(screen.queryByRole("button", { name: "Open a terminal" })).not.toBeInTheDocument();
    expect(await screen.findByText("No remote session yet")).toBeInTheDocument();
  });

  it("says an agent too old for terminals updates by itself", () => {
    const s = detail({ ...server({ command_kinds: {} }), remote_terminal: { can_open: true, can_review: false, recorded: false, idle_timeout_minutes: 15 } });
    render(
      <As user={operator}>
        <ServerTerminalTab server={s} />
      </As>,
    );
    expect(screen.getByText(/cannot open remote terminals yet/)).toBeInTheDocument();
    expect(screen.queryByRole("table")).not.toBeInTheDocument();
  });

  it("words session lengths", () => {
    expect(sessionLength(null)).toBe("Never opened");
    expect(sessionLength(40)).toBe("40 s");
    expect(sessionLength(673)).toBe("11 min");
    expect(sessionLength(3900)).toBe("1 h 5 min");
  });
});
