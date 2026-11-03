import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { Rollback, ServerDetail } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { RollbackFact, ServerRollback } from "./ServerRollback";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "0b7c5d6e-1f2a-4b3c-8d4e-5f6a7b8c9d0e";
const PATH = `GET /api/servers/${ID}/rollback`;
const server = { id: ID, hostname: "web-1", customer_id: "c1" } as ServerDetail;

const thin: Rollback = {
  server_id: ID,
  snapshots: {
    available: true,
    reason: "not_mapped",
    method: "lvm",
    detail: "LVM snapshot of / and /var (the thin pool has 40.0 GiB free)",
  },
  not_covered: ["/boot"],
  local_snapshots: [
    {
      name: "monitorflow-incident-1a2b3c4d5e6f-2",
      kind: "lvm_thin",
      created_at: new Date(Date.now() - 3_600_000).toISOString(),
      delete_after: "2026-11-10T10:15:00Z",
      volumes: ["root", "var"].map((lv) => ({
        name: "monitorflow-incident-1a2b3c4d5e6f-2",
        mount_point: lv === "root" ? "/" : "/var",
        kind: "lvm_thin" as const,
        source: `vg0/monitorflow-incident-1a2b3c4d5e6f-2_${lv}`,
        origin: `vg0/${lv}`,
        created_at: "2026-11-03T09:15:00Z",
        size_bytes: 0,
        used_percent: 0,
        invalid: false,
      })),
    },
  ],
  reported_at: "2026-11-03T10:00:00Z",
};

describe("RollbackFact", () => {
  it("names the local snapshot a change gets", async () => {
    stubFetch({ [PATH]: { status: 200, body: thin } });
    render(<RollbackFact server={server} canMap={false} />);

    expect(await screen.findByText("Snapshot first")).toBeInTheDocument();
    expect(
      screen.getByText(/LVM snapshot of \/ and \/var \(the thin pool has 40.0 GiB free\), taken by the agent before each AI change and update/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/\/boot is not covered: after rolling back a kernel update, pick the previous kernel/),
    ).toBeInTheDocument();
  });

  it("says plainly why rollback is limited", async () => {
    const plain = { ...thin, snapshots: { available: false, reason: "disabled" as const, detail: "/ is on plain ext4" } };
    stubFetch({ [PATH]: { status: 200, body: { ...plain, not_covered: [], local_snapshots: [] } } });
    render(<RollbackFact server={server} canMap={false} />);

    expect(await screen.findByText("Rollback limited")).toBeInTheDocument();
    expect(screen.getByText("/ is on plain ext4, so changes run without a snapshot.", { exact: false })).toBeInTheDocument();
  });

  it("leaves a Proxmox guest to Proxmox, without a local snapshot", async () => {
    const guest: Rollback = {
      ...thin,
      snapshots: {
        available: false,
        reason: "cannot_snapshot",
        kind: "lxc",
        detail: "rootfs is on storage local (dir), which cannot take snapshots",
      },
      not_covered: [],
    };
    stubFetch({ [PATH]: { status: 200, body: guest } });
    render(<RollbackFact server={server} canMap={false} />);

    expect(await screen.findByText("Rollback limited")).toBeInTheDocument();
    expect(screen.getByText(/The container's storage cannot take snapshots/)).toBeInTheDocument();
    expect(screen.queryByText(/LVM/)).not.toBeInTheDocument();
  });
});

describe("ServerRollback", () => {
  it("lists local snapshots and how to roll back by hand", async () => {
    stubFetch({ [PATH]: { status: 200, body: thin } });
    render(<ServerRollback server={server} />);

    const table = await screen.findByRole("table", { name: "Snapshots of web-1" });
    const row = within(table).getByText("monitorflow-incident-1a2b3c4d5e6f-2").closest("tr")!;
    expect(row).toHaveTextContent("/ and /var");
    expect(row).toHaveTextContent("Shares the thin pool");

    await userEvent.click(screen.getByText("How to roll back to monitorflow-incident-1a2b3c4d5e6f-2"));
    expect(screen.getByText(/lvconvert --merge vg0\/monitorflow-incident-1a2b3c4d5e6f-2_root/)).toHaveTextContent(
      "lvconvert --merge vg0/monitorflow-incident-1a2b3c4d5e6f-2_root lvconvert --merge vg0/monitorflow-incident-1a2b3c4d5e6f-2_var reboot",
    );
  });

  it("shows nothing without local snapshots", async () => {
    stubFetch({ [PATH]: { status: 200, body: { ...thin, local_snapshots: [] } } });
    const { container } = render(<ServerRollback server={server} />);
    await vi.waitFor(() => expect(container).toBeEmptyDOMElement());
  });
});
