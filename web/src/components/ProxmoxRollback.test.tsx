import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { ProxmoxView } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { ProxmoxRollback } from "./ProxmoxRollback";

const mapping: NonNullable<ProxmoxView["mapping"]> = {
  cluster: "pve",
  node: "pve2",
  vmid: 104,
  kind: "lxc",
  updated_by: null,
  updated_at: "2026-11-01T10:00:00Z",
  checked_at: "2026-11-01T10:01:00Z",
  cannot_snapshot: null,
  left_out: null,
};

function view(over: Partial<ProxmoxView> = {}): ProxmoxView {
  return {
    server_id: "s1",
    mapping,
    snapshots: {
      available: true,
      reason: null,
      kind: "lxc",
      detail: null,
      left_out: null,
    },
    clusters: ["pve"],
    ...over,
  };
}

describe("ProxmoxRollback", () => {
  it("says a container is snapshotted first, and what a snapshot leaves out", () => {
    render(
      <ProxmoxRollback
        view={view({
          snapshots: {
            available: true,
            reason: null,
            kind: "lxc",
            detail: null,
            left_out: "mp0 at /shared, a bind mount",
          },
        })}
        onChanged={() => {}}
      />,
    );
    expect(screen.getByText("Snapshot first")).toBeInTheDocument();
    expect(screen.getByText(/Proxmox snapshots container 104 on pve2 before each AI change/)).toBeInTheDocument();
    expect(
      screen.getByText(/Snapshots leave out mp0 at \/shared, a bind mount: a rollback does not restore it/),
    ).toBeInTheDocument();
  });

  it("says plainly when the container's storage cannot take snapshots, and why", () => {
    render(
      <ProxmoxRollback
        view={view({
          mapping: {
            ...mapping,
            cannot_snapshot: "rootfs is on storage local (dir), which cannot take snapshots",
          },
          snapshots: {
            available: false,
            reason: "cannot_snapshot",
            kind: "lxc",
            detail: "rootfs is on storage local (dir), which cannot take snapshots",
            left_out: null,
          },
        })}
        onChanged={() => {}}
      />,
    );
    expect(screen.getByText("Rollback limited")).toBeInTheDocument();
    expect(screen.getByText(/The container's storage cannot take snapshots\./)).toBeInTheDocument();
    expect(
      screen.getByText(
        "Container 104 on pve2, cluster pve: rootfs is on storage local (dir), which cannot take snapshots.",
      ),
    ).toBeInTheDocument();
  });

  it("maps an unmapped server to a container by hand", async () => {
    const saved = view();
    const fetchMock = stubFetch({
      "PUT /api/servers/s1/proxmox": { status: 200, body: saved },
    });
    const onChanged = vi.fn();
    render(
      <ProxmoxRollback
        view={view({
          mapping: null,
          snapshots: {
            available: false,
            reason: "not_mapped",
            kind: null,
            detail: null,
            left_out: null,
          },
        })}
        onChanged={onChanged}
      />,
    );
    expect(screen.getByText(/Not mapped to a Proxmox VM or container/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Map to a VM or container" }));
    await userEvent.selectOptions(screen.getByLabelText("Type"), "lxc");
    await userEvent.type(screen.getByLabelText("Node"), "pve2");
    await userEvent.type(screen.getByLabelText("ID"), "12");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(screen.getByRole("alert")).toHaveTextContent("The ID is a number from 100");
    await userEvent.type(screen.getByLabelText("ID"), "4");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(fetchMock.mock.calls.find((c) => c[1]?.method === "PUT")?.[1]?.body).toBe(
      JSON.stringify({ cluster: "pve", node: "pve2", vmid: 124, kind: "lxc" }),
    );
    expect(onChanged).toHaveBeenCalled();
  });

  it("explains a conflict and unmaps", async () => {
    stubFetch({
      "PUT /api/servers/s1/proxmox": {
        status: 409,
        body: { error: "the VM is already mapped to another server" },
      },
      "DELETE /api/servers/s1/proxmox": { status: 204 },
    });
    const onChanged = vi.fn();
    render(<ProxmoxRollback view={view()} onChanged={onChanged} />);
    await userEvent.click(screen.getByRole("button", { name: "Change" }));
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(screen.getByRole("alert")).toHaveTextContent("Another server is already mapped to this VM or container.");
    await userEvent.click(screen.getByRole("button", { name: "Unmap" }));
    expect(onChanged).toHaveBeenCalled();
  });

  it("says how discovery matched a server, and takes a correction", async () => {
    const fetchMock = stubFetch({
      "DELETE /api/servers/s1/proxmox": { status: 204 },
    });
    const onChanged = vi.fn();
    render(
      <ProxmoxRollback
        view={view({
          mapping: {
            ...mapping,
            kind: "qemu",
            matched_by: "uuid",
            name: "web-1",
          },
        })}
        onChanged={onChanged}
      />,
    );
    expect(
      screen.getByText(
        /Matched automatically to VM 104 web-1 on pve2 by its hardware ID\. Proxmox snapshots it before each AI change\./,
      ),
    ).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Not this one" }));
    expect(fetchMock.mock.calls[0]?.[1]?.method).toBe("DELETE");
    expect(onChanged).toHaveBeenCalled();
  });

  it("asks to check a match by name only", () => {
    render(
      <ProxmoxRollback
        view={view({
          mapping: { ...mapping, matched_by: "hostname", name: null },
        })}
        onChanged={() => {}}
      />,
    );
    expect(screen.getByText(/by its name only; check it is the right one/)).toBeInTheDocument();
  });

  it("asks which guest when several match", async () => {
    const fetchMock = stubFetch({
      "PUT /api/servers/s1/proxmox": { status: 200, body: view() },
    });
    const onChanged = vi.fn();
    const candidate = {
      cluster: "pve",
      node: "pve1",
      kind: "qemu" as const,
      matched_by: "mac" as const,
      taken: false,
    };
    render(
      <ProxmoxRollback
        view={view({
          mapping: null,
          snapshots: {
            available: false,
            reason: "not_mapped",
            kind: null,
            detail: null,
            left_out: null,
          },
          candidates: [
            { ...candidate, vmid: 101, name: "web-1" },
            { ...candidate, vmid: 102, name: "web-1-clone" },
          ],
        })}
        onChanged={onChanged}
      />,
    );
    expect(screen.getByText(/Several Proxmox VMs or containers match this server/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "VM 102 web-1-clone on pve1" }));
    expect(fetchMock.mock.calls[0]?.[1]?.body).toBe(
      JSON.stringify({ cluster: "pve", node: "pve1", vmid: 102, kind: "qemu" }),
    );
    expect(onChanged).toHaveBeenCalled();
    expect(screen.getByRole("button", { name: "Another one" })).toBeInTheDocument();
  });

  it("picks a guest from what discovery saw, or by hand", async () => {
    const fetchMock = stubFetch({
      "GET /api/servers/s1/proxmox/guests": {
        status: 200,
        body: {
          guests: [
            {
              cluster: "pve",
              node: "pve2",
              vmid: 104,
              kind: "lxc",
              name: "files-1",
              matched_by: null,
              taken: false,
            },
            {
              cluster: "pve",
              node: "pve1",
              vmid: 105,
              kind: "qemu",
              name: "db-1",
              matched_by: null,
              taken: true,
            },
          ],
        },
      },
      "PUT /api/servers/s1/proxmox": { status: 200, body: view() },
    });
    render(
      <ProxmoxRollback
        view={view({
          mapping: null,
          snapshots: {
            available: false,
            reason: "not_mapped",
            kind: null,
            detail: null,
            left_out: null,
          },
        })}
        onChanged={() => {}}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Map to a VM or container" }));
    await screen.findByLabelText("VM or container");
    expect(screen.queryByRole("option", { name: /db-1/ })).not.toBeInTheDocument();
    // A guest the list does not have: by hand.
    await userEvent.click(screen.getByRole("button", { name: "Not in the list" }));
    expect(screen.getByLabelText("Node")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await userEvent.click(screen.getByRole("button", { name: "Map to a VM or container" }));
    await userEvent.selectOptions(await screen.findByLabelText("VM or container"), "pve/104");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const put = fetchMock.mock.calls.find((c) => c[1]?.method === "PUT");
    expect(put?.[1]?.body).toBe(JSON.stringify({ cluster: "pve", node: "pve2", vmid: 104, kind: "lxc" }));
  });
});
