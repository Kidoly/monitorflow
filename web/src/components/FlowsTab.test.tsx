import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { FlowView, ServerDetail } from "@/lib/api";
import { As, me, oneSite } from "@/test/admin";
import { setUrl } from "@/test/navigation";

import { FlowsTab } from "./FlowsTab";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = oneSite(me({ role: "operator", customer_id: null, site_id: "site-1" }));
const viewer = oneSite(me({ role: "viewer", customer_id: null, site_id: "site-1" }));
const device = {
  id: "d1",
  hostname: "Core router",
  kind: "device",
  address: "192.0.2.1",
  site_id: "site-1",
  customer_id: "c1",
  revoked_at: null,
} as unknown as ServerDetail;
const probes = {
  probes: [
    { probe: "central", relay_id: null, key: "central", name: null, online: true, default: false },
    { probe: "relay", relay_id: "r1", key: "relay-paris", name: "relay-paris", online: true, default: true },
  ],
};
const exporter = {
  probe: "r1",
  relay_name: "relay-paris",
  source_address: null,
  address: "192.0.2.1",
  created_at: "2026-12-26T09:00:00Z",
  updated_at: "2026-12-26T09:00:00Z",
  last_flow_at: null,
};
const view = (address: string | null): FlowView => ({
  from: "2026-12-26T09:00:00Z",
  to: "2026-12-26T10:00:00Z",
  step_s: 60,
  address,
  bytes: 1000,
  packets: 10,
  bits_per_second: 2.2,
  dimensions: [
    { dimension: "src", keys: [{ key: "192.0.2.15", label: null, bytes: 800, packets: 8, bits_per_second: 1.8 }], other_bytes: 200 },
    { dimension: "app", keys: [{ key: "tcp/443", label: "HTTPS", bytes: 1000, packets: 10, bits_per_second: 2.2 }], other_bytes: 0 },
  ],
  chart: { by: "src", keys: [{ key: "192.0.2.15", label: null }], points: [{ at: "2026-12-26T09:00:00Z", values: [1.8], other: 0.4 }] },
});

type Reply = { status: number; body?: unknown };

/** Replies by method and path, the query left out; records what was asked. */
function stub(routes: (method: string, path: string, url: URL) => Reply) {
  const fetchMock = vi.fn<typeof fetch>(async (input, init) => {
    const url = new URL(String(input), "http://localhost");
    const reply = routes(init?.method ?? "GET", url.pathname, url);
    return new Response(reply.body === undefined ? null : JSON.stringify(reply.body), {
      status: reply.status,
      headers: { "Content-Type": "application/json" },
    });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

describe("FlowsTab", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setUrl("/servers/view/?id=d1&tab=flows");
  });

  it("sets up who receives the flows, the site's default probe first", async () => {
    let set = false;
    const fetchMock = stub((method, path) => {
      if (path === "/api/devices/d1/flow-exporter" && method === "PUT") {
        set = true;
        return { status: 200, body: exporter };
      }
      if (path === "/api/devices/d1/flow-exporter") return set ? { status: 200, body: exporter } : { status: 404, body: { error: "not_found" } };
      if (path === "/api/sites/site-1/probes") return { status: 200, body: probes };
      if (path === "/api/devices/d1/flows") return { status: 200, body: view(null) };
      if (path === "/api/servers/d1/thresholds") return { status: 200, body: { entries: [] } };
      throw new Error(`unexpected ${method} ${path}`);
    });
    render(
      <As user={operator}>
        <FlowsTab server={device} />
      </As>,
    );
    const user = userEvent.setup();
    await waitFor(() => expect((screen.getByLabelText("Received by") as HTMLSelectElement).value).toBe("r1"));
    expect(screen.getByText(/Only when its flows come from another address than 192\.0\.2\.1/)).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Receive flows" }));
    expect(await screen.findByText(/Received by relay relay-paris from 192\.0\.2\.1\./)).toBeTruthy();
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({ probe: "r1", source_address: null });
  });

  it("shows nothing to set up to a viewer", async () => {
    stub(() => ({ status: 404, body: { error: "not_found" } }));
    render(
      <As user={viewer}>
        <FlowsTab server={device} />
      </As>,
    );
    expect(await screen.findByText("Core router sends no traffic flows to MonitorFlow.")).toBeTruthy();
  });

  it("shows the tops and filters on a clicked address", async () => {
    const asked: string[] = [];
    stub((method, path, url) => {
      if (path === "/api/devices/d1/flow-exporter") return { status: 200, body: exporter };
      if (path === "/api/devices/d1/flows") {
        asked.push(url.searchParams.get("address") ?? "");
        return { status: 200, body: view(url.searchParams.get("address")) };
      }
      if (path === "/api/servers/d1/thresholds") return { status: 200, body: { entries: [] } };
      throw new Error(`unexpected ${method} ${path}`);
    });
    render(
      <As user={operator}>
        <FlowsTab server={device} />
      </As>,
    );
    const user = userEvent.setup();
    const apps = await screen.findByRole("table", { name: "Applications" });
    expect(within(apps).getByText("HTTPS (tcp/443)")).toBeTruthy();
    const sources = screen.getByRole("table", { name: "Sources" });
    expect(within(sources).getByText("Other")).toBeTruthy();
    await user.click(within(sources).getByRole("button", { name: "Show only the traffic of 192.0.2.15" }));
    expect(await screen.findByText("Only 192.0.2.15")).toBeTruthy();
    await waitFor(() => expect(asked).toContain("192.0.2.15"));
    expect(window.location.search).toContain("address=192.0.2.15");
  });

  it("adds an alert on any address above a rate", async () => {
    const fetchMock = stub((method, path) => {
      if (path === "/api/devices/d1/flow-exporter") return { status: 200, body: exporter };
      if (path === "/api/devices/d1/flows") return { status: 200, body: view(null) };
      if (path === "/api/servers/d1/thresholds" && method === "POST") return { status: 201, body: { id: "t1" } };
      if (path === "/api/servers/d1/thresholds") return { status: 200, body: { entries: [] } };
      throw new Error(`unexpected ${method} ${path}`);
    });
    render(
      <As user={operator}>
        <FlowsTab server={device} />
      </As>,
    );
    const user = userEvent.setup();
    const rate = await screen.findByLabelText("Above (Mbit/s)");
    await user.clear(rate);
    await user.type(rate, "50");
    await user.click(screen.getByRole("button", { name: "Add the alert" }));
    expect(await screen.findByText("Alert added.")).toBeTruthy();
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toMatchObject({
      metric: "monitorflow_flow_address_bits_per_second",
      instance: null,
      comparison: ">",
      warning: 50_000_000,
      duration_s: 300,
    });
  });
});
