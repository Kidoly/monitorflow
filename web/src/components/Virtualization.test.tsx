import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { ServerDetail, VirtualObject } from "@/lib/api";
import { As, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { router } from "@/test/navigation";

import { HYPERV_GROUP_COMMAND, VirtualizationSection, objectState, snapshots } from "./Virtualization";

vi.mock("next/navigation", () => import("@/test/navigation"));

const NOW = new Date();
const server = (overrides: Partial<ServerDetail> = {}): ServerDetail =>
  ({
    id: "vc1",
    peer_id: "vcenter-1",
    hostname: "vcenter-1",
    site_id: "site-1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    kind: "device",
    os_family: "device",
    address: "192.0.2.40",
    revoked_at: null,
    ...overrides,
  }) as ServerDetail;

const object = (overrides: Partial<VirtualObject>): VirtualObject => ({
  id: "o1",
  kind: "vm",
  name: "web-01",
  key: "vm-1",
  details: {},
  metrics: {},
  collected_at: NOW.toISOString(),
  seen_at: NOW.toISOString(),
  stale: false,
  server: null,
  matched_by: null,
  link_fixed: false,
  ...overrides,
});

const host = object({ id: "h1", kind: "host", name: "esx-01", details: { connection_state: "disconnected", cluster: "prod" }, metrics: { connected: 0 } });
const web = object({
  id: "v1",
  name: "web-01",
  details: { host: "esx-01", power_state: "poweredOn" },
  metrics: { powered_on: 1, cpu_count: 2, cpu_usage_ratio: 0.25, snapshots: 2, snapshot_age_seconds: 9 * 86_400 },
  server: { id: "s1", hostname: "web-01", kind: "agent", awaiting_agent: false },
  matched_by: "uuid",
});
const build = object({ id: "v2", name: "build", details: { host: "esx-01" }, metrics: { powered_on: 0, snapshots: 0 } });
const ds = object({ id: "d1", kind: "datastore", name: "ds1", metrics: { accessible: 1, free_bytes: 2 * 1024 ** 3, free_ratio: 0.02, capacity_bytes: 100 * 1024 ** 3 } });

const inventory = {
  source: { up: true, reason: null, error: null, details: { product: "VMware vCenter Server 8.0.2" }, seen_at: NOW.toISOString() },
  objects: [host, web, build, ds],
  guest_of: [],
};

describe("Virtualization", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    router.push.mockClear();
  });

  it("says states and snapshots in words", () => {
    expect(objectState(host, NOW)).toBe("Disconnected");
    expect(objectState(web, NOW)).toBe("Running");
    expect(objectState(ds, NOW)).toBe("Available");
    expect(objectState(object({ stale: true, seen_at: new Date(NOW.getTime() - 3_600_000).toISOString() }), NOW)).toMatch(/^Not seen since/);
    expect(snapshots(build)).toBe("None");
    expect(snapshots(web)).toMatch(/^2, oldest /);
  });

  it("lists a vCenter's inventory and installs the agent on a virtual machine without one", async () => {
    const fetchMock = stubFetch({
      "GET /api/servers/vc1/virtualization": { status: 200, body: inventory },
      "POST /api/virtual-objects/v2/install-agent": { status: 201, body: { id: "new-1" } },
    });
    render(
      <As user={me({ role: "administrator", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server()} />
      </As>,
    );
    expect(await screen.findByText(/VMware vCenter Server 8.0.2/)).toBeTruthy();
    expect(screen.getByRole("table", { name: "Hosts" }).textContent).toContain("Disconnected");
    const vms = screen.getByRole("table", { name: "Virtual machines" }).textContent ?? "";
    expect(vms).toContain("matched by its hardware ID");
    expect(vms).toContain("No agent");
    expect(screen.getByRole("table", { name: "Datastores" }).textContent).toContain("2%");
    // Only the virtual machine without a server offers it.
    const install = screen.getAllByRole("button", { name: "Install the agent" });
    expect(install).toHaveLength(1);
    await userEvent.setup().click(screen.getByRole("button", { name: "Install the agent" }));
    await waitFor(() => expect(router.push).toHaveBeenCalledWith("/servers/view/?id=new-1"));
    expect(fetchMock.mock.calls.some(([url, init]) => String(url).endsWith("/v2/install-agent") && init?.method === "POST")).toBe(true);
  });

  it("lets an operator undo a match but not install the agent", async () => {
    const fetchMock = stubFetch({
      "GET /api/servers/vc1/virtualization": { status: 200, body: inventory },
      "PUT /api/virtual-objects/v1/server": { status: 200, body: web },
    });
    render(
      <As user={me({ role: "operator", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server()} />
      </As>,
    );
    const notThis = await screen.findByRole("button", { name: "Not this one" });
    expect(screen.queryByRole("button", { name: "Install the agent" })).toBeNull();
    await userEvent.setup().click(notThis);
    await waitFor(() => {
      const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
      expect(JSON.parse(String(put?.[1]?.body))).toEqual({ server_id: null });
    });
  });

  it("shows the virtual machine a server is", async () => {
    stubFetch({
      "GET /api/servers/s1/virtualization": {
        status: 200,
        body: { source: null, objects: [], guest_of: [{ object: web, source_id: "vc1", source_name: "vcenter-1" }] },
      },
    });
    render(
      <As user={me({ role: "viewer", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server({ id: "s1", kind: "agent", os_family: "debian" })} />
      </As>,
    );
    expect(await screen.findByText(/Virtual machine web-01 on host esx-01 of vcenter-1, matched by its hardware ID/)).toBeTruthy();
    expect(screen.getByRole("link", { name: "vcenter-1" })).toBeTruthy();
  });

  it("offers to watch Hyper-V on a Windows server, with the group to join", async () => {
    const fetchMock = stubFetch({
      "GET /api/servers/hv1/virtualization": { status: 200, body: { source: null, objects: [], guest_of: [] } },
      "POST /api/servers/hv1/hyperv": { status: 200, body: { added: true, check: "hyperv" } },
    });
    render(
      <As user={me({ role: "operator", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server({ id: "hv1", kind: "agent", os_family: "windows" })} />
      </As>,
    );
    const user = userEvent.setup();
    await user.click(await screen.findByText("Watch Hyper-V", { selector: "summary" }));
    expect(screen.getByText(HYPERV_GROUP_COMMAND)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Watch Hyper-V" }));
    expect(await screen.findByText(/Hyper-V is watched/)).toBeTruthy();
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(true);
  });

  it("says why a source could not be read", async () => {
    stubFetch({
      "GET /api/servers/hv1/virtualization": {
        status: 200,
        body: { source: { up: false, reason: "denied", error: "the agent cannot read Hyper-V", details: {}, seen_at: NOW.toISOString() }, objects: [], guest_of: [] },
      },
    });
    render(
      <As user={me({ role: "viewer", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server({ id: "hv1", kind: "agent", os_family: "windows" })} hypervWatched />
      </As>,
    );
    expect(await screen.findByText(/Could not be read .*the agent cannot read Hyper-V/)).toBeTruthy();
    expect(screen.queryByText("Watch Hyper-V")).toBeNull();
  });
  it("lists a Kubernetes cluster's nodes, namespaces, workloads and failed jobs", async () => {
    const old = new Date(NOW.getTime() - 10 * 60_000).toISOString();
    const worker = object({
      id: "n1",
      kind: "node",
      name: "worker-1",
      details: { roles: "worker", kubelet_version: "v1.31.4" },
      metrics: { ready: 1, cpu_cores: 4, cpu_usage_ratio: 0.25, memory_bytes: 8 * 1024 ** 3, memory_usage_ratio: 0.5, pods: 12, pods_capacity: 110 },
      server: { id: "s2", hostname: "worker-1", kind: "agent", awaiting_agent: false },
      matched_by: "uuid",
    });
    const spare = object({ id: "n2", kind: "node", name: "spare-1", metrics: { ready: 0 } });
    const shop = object({ id: "ns1", kind: "namespace", name: "shop", details: { phase: "Active" }, metrics: { pods: 3, pods_not_running: 1, jobs_failed: 1, cpu_usage_cores: 0.3, memory_usage_bytes: 96 * 1024 ** 2 } });
    const deploy = object({
      id: "w1",
      kind: "workload",
      name: "shop/deployment/web",
      details: { namespace: "shop", workload: "deployment", name: "web" },
      metrics: { replicas_desired: 3, replicas_ready: 2, ready_ratio: 2 / 3 },
    });
    const job = object({ id: "j1", kind: "job", name: "shop/migrate", details: { namespace: "shop", reason: "BackoffLimitExceeded", message: "Job has reached the specified backoff limit" } });
    // Failed at an earlier reading only: it no longer fails.
    const healed = object({ id: "j2", kind: "job", name: "shop/backup-1", details: { namespace: "shop", cron_job: "backup" }, seen_at: old });
    stubFetch({
      "GET /api/servers/k1/virtualization": {
        status: 200,
        body: {
          source: { up: true, reason: null, error: null, details: { product: "Kubernetes", version: "v1.31.4", unread: "metrics-server pods (not allowed)" }, seen_at: NOW.toISOString() },
          objects: [worker, spare, shop, deploy, job, healed],
          guest_of: [],
        },
      },
    });
    render(
      <As user={me({ role: "administrator", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server({ id: "k1", hostname: "k8s-1" })} />
      </As>,
    );
    expect(await screen.findByText(/Kubernetes v1\.31\.4/)).toBeTruthy();
    expect(screen.getByText(/Not read: metrics-server pods \(not allowed\)/)).toBeTruthy();
    const nodes = screen.getByRole("table", { name: "Nodes" }).textContent ?? "";
    for (const shown of ["worker-1", "Ready", "4 cores, 25% used", "12 of 110", "matched by its hardware ID", "Not ready"]) expect(nodes).toContain(shown);
    expect(screen.getAllByRole("button", { name: "Install the agent" })).toHaveLength(1);
    const spaces = screen.getByRole("table", { name: "Namespaces" }).textContent ?? "";
    expect(spaces).toContain("3, 1 not running");
    expect(spaces).toContain("0.3 cores, 96 MiB");
    const workloads = screen.getByRole("table", { name: "Workloads" }).textContent ?? "";
    for (const shown of ["web", "Deployment", "2 of 3", "Short of replicas"]) expect(workloads).toContain(shown);
    const jobs = screen.getByRole("table", { name: "Failed jobs" }).textContent ?? "";
    expect(jobs).toContain("migrate");
    expect(jobs).toContain("BackoffLimitExceeded");
    expect(jobs).not.toContain("backup-1");
    expect(screen.queryByRole("table", { name: "Virtual machines" })).toBeNull();
  });

  it("shows the Kubernetes node a server is", async () => {
    const node = object({ id: "n1", kind: "node", name: "worker-1", matched_by: "name" });
    stubFetch({
      "GET /api/servers/s2/virtualization": { status: 200, body: { source: null, objects: [], guest_of: [{ object: node, source_id: "k1", source_name: "k8s-1" }] } },
    });
    render(
      <As user={me({ role: "viewer", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server({ id: "s2", kind: "agent", os_family: "debian" })} />
      </As>,
    );
    expect(await screen.findByText(/Kubernetes node worker-1 of k8s-1, matched by its name only/)).toBeTruthy();
    expect(screen.getByRole("link", { name: "k8s-1" })).toBeTruthy();
  });

  it("lists a cloud account's machines with their network, disks and status checks", async () => {
    const ec2 = object({
      id: "i1",
      name: "web",
      key: "i-0123456789abcdef0",
      details: { host: "eu-west-3a", size: "t3.micro", power_state: "running" },
      metrics: { powered_on: 1, cpu_count: 2, cpu_usage_ratio: 0.5, network_in_bytes_per_second: 2048, network_out_bytes_per_second: 512, status_checks_failed: 1 },
    });
    const stopped = object({ id: "i2", name: "batch", details: { power_state: "stopped" }, metrics: { powered_on: 0 } });
    stubFetch({
      "GET /api/servers/aws1/virtualization": {
        status: 200,
        body: {
          source: { up: true, reason: null, error: null, details: { product: "Amazon EC2 (eu-west-3)", provider: "aws" }, seen_at: NOW.toISOString() },
          objects: [ec2, stopped],
          guest_of: [],
        },
      },
    });
    render(
      <As user={me({ role: "administrator", customer_id: null, site_id: null })}>
        <VirtualizationSection server={server({ id: "aws1", address: "ec2.eu-west-3.amazonaws.com" })} />
      </As>,
    );
    expect(await screen.findByText(/Amazon EC2 \(eu-west-3\)/)).toBeTruthy();
    const table = screen.getByRole("table", { name: "Virtual machines" });
    const text = table.textContent ?? "";
    for (const shown of ["Zone", "eu-west-3a", "t3.micro", "2 KiB/s in, 512 B/s out", "1 failed", "Not checked"]) expect(text).toContain(shown);
    expect(text).not.toContain("Snapshots");
  });
});
