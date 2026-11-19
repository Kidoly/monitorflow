import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, me, oneSite } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { NetworkScanView } from "./NetworkScan";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = oneSite(me({ role: "operator", customer_id: null, site_id: "site-1" }));
const site = { id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme" };
const run = {
  id: "r2", state: "done", trigger: "schedule", queued_at: "2026-11-18T10:00:00Z", started_at: null,
  finished_at: "2026-11-18T10:02:00Z", addresses: 254, scanned: 254, answered: 4, compared: true, new: 1,
  gone: ["192.0.2.40"], dropped: 0, ping_error: null, error: null,
};
const scan = {
  id: "s1", site_id: "site-1", site_name: "paris", name: "Office", targets: ["192.0.2.0/24"], addresses: 254,
  ports: [22, 80], probe: "relay", probe_relay_id: "r1", probe_key: "relay-paris", every: null, notify: false,
  snmp: null, created_at: "2026-11-18T10:00:00Z", updated_at: "2026-11-18T10:00:00Z", last_run: run,
};
const base = {
  ping: true, rtt_seconds: 0.001, mac: null, snmp: false, snmp_description: null, snmp_name: null,
  snmp_object_id: null, snmp_groups: [], new: false,
};
const results = {
  run,
  current: null,
  hosts: [
    { ...base, address: "192.0.2.10", ports: [22], name: "web-2.lan", new: true,
      proposal: { action: "agent", os: "linux", monitored_by: null, name: "web-2.lan", template: null,
                  checks: [{ kind: "ping", name: "ping", port: null }, { kind: "port", name: "port_22", port: 22 }] } },
    { ...base, address: "192.0.2.20", ports: [80], name: null, snmp: true, snmp_description: "UPS card",
      snmp_name: "ups-1", snmp_groups: ["ups"],
      proposal: { action: "snmp_device", os: null, monitored_by: null, name: "ups-1", template: "ups",
                  checks: [{ kind: "ping", name: "ping", port: null }, { kind: "snmp", name: "snmp_ups", port: null }] } },
    { ...base, address: "192.0.2.30", ports: [443], name: "shop.lan",
      proposal: { action: "device", os: null, monitored_by: null, name: "shop.lan", template: null,
                  checks: [{ kind: "port", name: "port_443", port: 443 }] } },
    { ...base, address: "192.0.2.5", ports: [], name: null,
      proposal: { action: "monitored", os: null, monitored_by: { server_id: "x1", name: "Core switch", matched_by: "address" },
                  name: "x", template: null, checks: [] } },
  ],
};

describe("NetworkScanView", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setUrl("/discovery/view/?id=s1");
  });

  it("shows what was found, proposes, and adds every device at once", async () => {
    const fetchMock = stubFetch({
      "GET /api/network-scans/s1": { status: 200, body: scan },
      "GET /api/network-scans/s1/results": { status: 200, body: results },
      "GET /api/sites": { status: 200, body: { sites: [site] } },
      "POST /api/network-scans/s1/add": {
        status: 200,
        body: { added: [{ address: "192.0.2.20", server_id: "d1", name: "ups-1", template: "ups" },
                        { address: "192.0.2.30", server_id: "d2", name: "shop.lan", template: null }], skipped: [] },
      },
    });
    render(
      <As user={operator}>
        <NetworkScanView />
      </As>,
    );
    expect(await screen.findByText("Install the agent: a Linux or Unix server, SSH is open")).toBeTruthy();
    expect(screen.getByText("Add as a device: UPS over SNMP")).toBeTruthy();
    expect(screen.getByText("Watched as Core switch (same address)")).toBeTruthy();
    expect(screen.getByText(/Since the scan before: 1 new, 1 gone\./)).toBeTruthy();
    expect(screen.getByText("Gone since the scan before: 192.0.2.40")).toBeTruthy();
    expect(screen.getByText("New")).toBeTruthy();
    expect(screen.getByText("It gets: ping, UPS battery")).toBeTruthy();
    const proposed = screen.getByRole("table", { name: "Proposed" });
    expect(within(proposed).getByRole("link", { name: "Install the agent" }).getAttribute("href")).toMatch(/^\/settings\/add-servers/);
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: "Add all 2 proposed devices" }));
    await waitFor(() => expect(screen.getByText("2 devices added.")).toBeTruthy());
    const call = fetchMock.mock.calls.find(([url, init]) => String(url).endsWith("/add") && init?.method === "POST");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ all: true });
  });

  it("adds one with another template, and dismisses one", async () => {
    const fetchMock = stubFetch({
      "GET /api/network-scans/s1": { status: 200, body: scan },
      "GET /api/network-scans/s1/results": { status: 200, body: results },
      "GET /api/sites": { status: 200, body: { sites: [site] } },
      "POST /api/network-scans/s1/add": { status: 200, body: { added: [], skipped: [{ address: "192.0.2.20", reason: "monitored" }] } },
      "POST /api/network-scans/s1/dismiss": { status: 204 },
    });
    render(
      <As user={operator}>
        <NetworkScanView />
      </As>,
    );
    const user = userEvent.setup();
    await user.selectOptions(await screen.findByLabelText("Template"), "nas");
    await user.click(screen.getByRole("button", { name: "Add 192.0.2.20" }));
    await waitFor(() => expect(screen.getByText("192.0.2.20 is already monitored.")).toBeTruthy());
    const add = fetchMock.mock.calls.find(([url]) => String(url).endsWith("/add"));
    expect(JSON.parse(String(add?.[1]?.body))).toEqual({ devices: [{ address: "192.0.2.20", template: "nas" }] });
    await user.click(screen.getByRole("button", { name: "Dismiss 192.0.2.30" }));
    await waitFor(() =>
      expect(fetchMock.mock.calls.some(([url]) => String(url).endsWith("/dismiss"))).toBe(true),
    );
  });

  it("says a relay is waited for and a run failed in words", async () => {
    stubFetch({
      "GET /api/network-scans/s1": { status: 200, body: { ...scan, last_run: { ...run, state: "queued" } } },
      "GET /api/network-scans/s1/results": {
        status: 200, body: { run: null, current: { ...run, state: "queued" }, hosts: [] } },
      "GET /api/sites": { status: 200, body: { sites: [site] } },
    });
    render(
      <As user={operator}>
        <NetworkScanView />
      </As>,
    );
    expect(await screen.findByText("Waiting for relay relay-paris: the scan starts when it is connected")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Scan now" }) as HTMLButtonElement).disabled).toBe(true);
  });
});
