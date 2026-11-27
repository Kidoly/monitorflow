import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { RemoteSession, ServerDetail } from "@/lib/api";
import { As, me, siteAdmin } from "@/test/admin";
import { server } from "@/test/campaigns";
import { stubFetch } from "@/test/fetch";

import { describeDesktopError } from "./RemoteDesktop";
import { ServerDesktopTab } from "./ServerDesktopTab";

vi.mock("next/navigation", () => import("@/test/navigation"));
// IronRDP's client needs a real browser: the live desktop is a stand-in here.
vi.mock("next/dynamic", () => ({
  default: () =>
    function Stub({
      serverId,
      credentials,
      sessionId,
    }: {
      serverId?: string;
      credentials?: { username: string; password: string };
      sessionId?: string;
    }) {
      if (sessionId) return <div data-testid="replay">{sessionId}</div>;
      return (
        <div data-testid="desktop">
          {serverId} {credentials?.username} {credentials?.password}
        </div>
      );
    },
}));

const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });

function detail(overrides: Partial<ServerDetail> = {}): ServerDetail {
  return {
    ...server({ os_family: "windows", command_kinds: { remote_session: "off", remote_desktop: "accepted" } }),
    certificate: null,
    remote_terminal: { can_open: true, can_review: true, recorded: true, idle_timeout_minutes: 15 },
    ...overrides,
  };
}

const session = (overrides: Partial<RemoteSession> = {}): RemoteSession => ({
  id: "d1",
  server_id: "web",
  hostname: "web-1",
  kind: "desktop",
  requested_by: "bob",
  created_at: "2026-11-23T09:30:00Z",
  opened_at: "2026-11-23T09:30:02Z",
  closed_at: "2026-11-23T09:41:15Z",
  close_reason: "closed after 15 minutes without input",
  duration_seconds: 673,
  recorded: false,
  bytes_in: 10,
  bytes_out: 100,
  ...overrides,
});

beforeEach(() => {
  setActiveLocale("en");
  vi.unstubAllGlobals();
});

describe("ServerDesktopTab", () => {
  it("signs in with a Windows account and lists only desktop sessions", async () => {
    const s = detail();
    stubFetch({
      [`GET /api/servers/${s.id}/remote-sessions`]: {
        status: 200,
        body: [
          session(),
          session({ id: "d2", recorded: true, close_reason: "closed by dave" }),
          session({ id: "t1", kind: "terminal", close_reason: "closed by carol" }),
        ],
      },
    });
    render(
      <As user={siteAdmin}>
        <ServerDesktopTab server={s} />
      </As>,
    );
    expect(screen.getByText(/closes after 15 minutes without input\. What it shows is recorded\./)).toBeInTheDocument();
    const table = await screen.findByRole("table", { name: "Remote desktop sessions" });
    expect(within(table).getByText("closed after 15 minutes without input")).toBeInTheDocument();
    expect(within(table).queryByText("closed by carol")).not.toBeInTheDocument();
    expect(within(table).getByText("Recorded")).toBeInTheDocument();
    // Only the recorded session has a replay, for the site's administrators.
    const replays = within(table).getAllByRole("button", { name: "Replay" });
    expect(replays).toHaveLength(1);
    await userEvent.click(replays[0]!);
    expect(screen.getByTestId("replay")).toHaveTextContent("d2");

    await userEvent.type(screen.getByLabelText("Account"), "Administrator");
    await userEvent.type(screen.getByLabelText("Password"), "s3cret");
    await userEvent.click(screen.getByRole("button", { name: "Connect" }));
    expect(screen.getByTestId("desktop")).toHaveTextContent(`${s.id} Administrator s3cret`);
    expect(screen.queryByLabelText("Password")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Close the session" })).toBeInTheDocument();
  });

  it("gives administrators the lines that allow desktops where the server does not", async () => {
    const s = detail({ ...server({ os_family: "windows", command_kinds: { remote_session: "accepted", remote_desktop: "off" } }) });
    stubFetch({ [`GET /api/servers/${s.id}/remote-sessions`]: { status: 200, body: [] } });
    render(
      <As user={siteAdmin}>
        <ServerDesktopTab server={s} />
      </As>,
    );
    expect(screen.getByText(/does not allow remote desktops/)).toBeInTheDocument();
    expect(screen.getByLabelText("Lines that allow remote desktops")).toHaveTextContent("remote-access desktop on");
    expect(screen.queryByRole("button", { name: "Connect" })).not.toBeInTheDocument();
    expect(await screen.findByText("No remote session yet")).toBeInTheDocument();
  });

  it("tells others the server's administrator allows it, and an old agent that it updates", () => {
    const off = detail({ ...server({ os_family: "windows", command_kinds: { remote_desktop: "off" } }) });
    const { unmount } = render(
      <As user={operator}>
        <ServerDesktopTab server={{ ...off, remote_terminal: { ...off.remote_terminal!, can_review: false } }} />
      </As>,
    );
    expect(screen.getByText(/administrator can allow them on the server itself/)).toBeInTheDocument();
    expect(screen.queryByLabelText("Lines that allow remote desktops")).not.toBeInTheDocument();
    unmount();
    const old = detail({ ...server({ os_family: "windows", command_kinds: {} }) });
    render(
      <As user={operator}>
        <ServerDesktopTab server={{ ...old, remote_terminal: { ...old.remote_terminal!, can_review: false } }} />
      </As>,
    );
    expect(screen.getByText(/cannot open remote desktops yet/)).toBeInTheDocument();
  });

  it("words the client's errors", () => {
    const iron = (kind: number) => ({ kind: () => kind, backtrace: () => "", rdcleanpathDetails: () => undefined });
    expect(describeDesktopError(iron(1))).toBe("Windows refused this account or password");
    expect(describeDesktopError(iron(3))).toMatch(/may not sign in through Remote Desktop/);
    expect(describeDesktopError(iron(4))).toBe("the central server refused the connection");
    expect(describeDesktopError(iron(0))).toBe("the connection ended");
  });
});
