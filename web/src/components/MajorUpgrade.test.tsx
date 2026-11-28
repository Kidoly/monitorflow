import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { ServerDetail, ServerUpgrade } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { ServerMajorUpgrade } from "./MajorUpgrade";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "6f1c2a7e-3b4d-4e8f-9a0b-1c2d3e4f5a6b";
const LIST = `GET /api/major-upgrades?server_id=${ID}`;
const server = { id: ID, hostname: "db-1", customer_id: "c1" } as ServerDetail;

const rocky: ServerUpgrade = {
  server_id: ID,
  hostname: "db-1",
  site_id: "s1",
  customer_id: "c1",
  release: "rocky 8.10",
  release_label: "Rocky Linux 8",
  target: "rocky 9",
  target_label: "Rocky Linux 9",
  ready: true,
  checks: [{ name: "leapp", state: "ok", detail: "leapp and its data for Rocky Linux 9 are installed", items: [] }],
  rollback: { available: true, reason: null, detail: "LVM snapshot of / and /var" },
  progress: null,
  reported_at: null,
  can_upgrade: true,
  can_preupgrade: true,
};

describe("ServerMajorUpgrade", () => {
  it("runs leapp preupgrade alone after a confirmation", async () => {
    const fetchMock = stubFetch({
      [LIST]: { status: 200, body: { servers: [rocky] } },
      [`POST /api/servers/${ID}/major-upgrade/preupgrade`]: { status: 201, body: { id: "cmd-1", kind: "leapp_preupgrade" } },
    });
    render(<ServerMajorUpgrade server={server} />);

    await userEvent.click(await screen.findByRole("button", { name: "Run leapp preupgrade" }));
    expect(screen.getByText("Run leapp preupgrade on db-1?")).toBeInTheDocument();
    expect(screen.getByText(/Nothing changes on the server/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Run leapp preupgrade" }));

    const posted = fetchMock.mock.calls.some(
      ([url, init]) => String(url) === `/api/servers/${ID}/major-upgrade/preupgrade` && init?.method === "POST",
    );
    expect(posted).toBe(true);
  });

  it("shows the report of the last leapp preupgrade, and no button while one runs", async () => {
    const done = {
      ...rocky,
      progress: {
        command_id: "cmd-1",
        from: "rocky 8",
        to: "rocky 9",
        phase: "done" as const,
        error: null,
        sources_restored: false,
        disabled_sources: [],
        report: "Risk Factor: info\nTitle: Packages not signed by Rocky\n",
        preupgrade_only: true,
        started_at: null,
        updated_at: null,
      },
    };
    stubFetch({ [LIST]: { status: 200, body: { servers: [done] } } });
    const { unmount } = render(<ServerMajorUpgrade server={server} />);
    expect(await screen.findByText("Last leapp preupgrade")).toBeInTheDocument();
    expect(screen.getByText("No inhibitor")).toBeInTheDocument();
    expect(screen.getByText(/Packages not signed by Rocky/)).toBeVisible();
    expect(screen.getByRole("button", { name: "Run leapp preupgrade" })).toBeInTheDocument();
    unmount();

    const running = { ...done, progress: { ...done.progress, phase: "preupgrade" as const } };
    stubFetch({ [LIST]: { status: 200, body: { servers: [running] } } });
    render(<ServerMajorUpgrade server={server} />);
    expect(await screen.findByText("Checking with leapp")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Run leapp preupgrade" })).not.toBeInTheDocument();
  });
});
