import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, me, oneSite } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { router } from "@/test/navigation";

import { AddDevice, probeLabel, validAddress } from "./AddDevice";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = oneSite(me({ role: "operator", customer_id: null, site_id: "site-1" }));
const site = { id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme" };
const probes = {
  probes: [
    { probe: "central", relay_id: null, key: "central", name: null, online: true, default: false },
    { probe: "relay", relay_id: "r1", key: "relay-paris", name: "relay-paris", online: true, default: true },
  ],
};
const sent = (fetchMock: ReturnType<typeof stubFetch>) =>
  JSON.parse(String(fetchMock.mock.calls.find(([, init]) => init?.method === "POST")?.[1]?.body));

describe("AddDevice", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("checks addresses as the server does", () => {
    for (const ok of ["192.0.2.5", "nas.lan", "2001:db8::1", "[2001:db8::1]", "core_switch"]) expect(validAddress(ok), ok).toBe(true);
    for (const bad of ["", "http://x", "a b", "x/y", "nas.lan:22", "a..b"]) expect(validAddress(bad), bad).toBe(false);
  });

  it("names probes and marks the default", () => {
    expect(probeLabel(probes.probes[0] as never)).toBe("The central server");
    expect(probeLabel(probes.probes[1] as never)).toBe("Relay relay-paris (recommended)");
    expect(probeLabel({ ...probes.probes[1], online: false, default: false } as never)).toBe("Relay relay-paris (not connected now)");
  });

  it("adds a device pinged from the default probe, then opens its page", async () => {
    const fetchMock = stubFetch({
      "GET /api/sites": { status: 200, body: { sites: [site] } },
      "GET /api/sites/site-1/probes": { status: 200, body: probes },
      "POST /api/devices": { status: 201, body: { id: "d1" } },
    });
    render(
      <As user={operator}>
        <AddDevice />
      </As>,
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Add the device" }));
    expect(screen.getByText("Give it a name.")).toBeTruthy();
    await user.type(screen.getByLabelText(/^Name/), "Core switch");
    await user.type(screen.getByLabelText(/^Address/), "http://switch");
    await user.click(screen.getByRole("button", { name: "Add the device" }));
    expect(screen.getByText(/without http:\/\/ or a port/)).toBeTruthy();
    await user.clear(screen.getByLabelText(/^Address/));
    await user.type(screen.getByLabelText(/^Address/), "192.0.2.10");
    await waitFor(() => expect((screen.getByLabelText("Checked from") as HTMLSelectElement).value).toBe("r1"));
    await user.click(screen.getByRole("button", { name: "Add the device" }));
    await waitFor(() => expect(router.push).toHaveBeenCalledWith("/servers/view/?id=d1"));
    expect(sent(fetchMock)).toEqual({ site_id: "site-1", name: "Core switch", address: "192.0.2.10", ping: true, probe: "r1" });
  });

  it("detects what a device answers over SNMP and adds it with its template", async () => {
    const fetchMock = stubFetch({
      "GET /api/sites": { status: 200, body: { sites: [site] } },
      "GET /api/sites/site-1/probes": { status: 200, body: probes },
      "GET /api/devices/templates": {
        status: 200,
        body: { templates: ["switch", "ups", "nas"].map((key) => ({ key, groups: ["system"], thresholds: [] })) },
      },
      "POST /api/devices/detect": {
        status: 200,
        body: { answers: true, error: null, description: "Test UPS", name: "ups-room", object_id: "1.3.6.1.2.1.33",
                groups: ["ups"], template: "ups", probe: "relay-paris" },
      },
      "POST /api/devices": { status: 201, body: { id: "d2" } },
    });
    render(
      <As user={operator}>
        <AddDevice />
      </As>,
    );
    const user = userEvent.setup();
    await user.type(await screen.findByLabelText(/^Address/), "192.0.2.30");
    await user.click(screen.getByLabelText(/Watch it over SNMP/));
    await user.click(screen.getByRole("button", { name: "Detect" }));
    expect(await screen.findByText("Enter the community.")).toBeTruthy();
    await user.type(screen.getByLabelText(/^Community/), "e2e-community");
    await waitFor(() => expect((screen.getByLabelText("Checked from") as HTMLSelectElement).value).toBe("r1"));
    await user.click(screen.getByRole("button", { name: "Detect" }));
    expect(await screen.findByText(/ups-room answers: Test UPS/)).toBeTruthy();
    expect(screen.getByText(/It reports: UPS battery/)).toBeTruthy();
    expect((screen.getByLabelText(/^Kind of device/) as HTMLSelectElement).value).toBe("ups");
    expect((screen.getByLabelText(/^Name/) as HTMLInputElement).value).toBe("ups-room");
    await user.click(screen.getByRole("button", { name: "Add the device" }));
    await waitFor(() => expect(router.push).toHaveBeenCalledWith("/servers/view/?id=d2"));
    const added = JSON.parse(String(fetchMock.mock.calls.filter(([url, init]) => url === "/api/devices" && init?.method === "POST")[0]![1]!.body));
    expect(added).toMatchObject({ template: "ups", probe: "r1", snmp: { version: "v2c", community: "e2e-community", port: 161 } });
  });

  it("says so when no site can take a device", async () => {
    stubFetch({ "GET /api/sites": { status: 200, body: { sites: [site] } } });
    render(
      <As user={oneSite(me({ role: "viewer", customer_id: null, site_id: "site-1" }))}>
        <AddDevice />
      </As>,
    );
    expect(await screen.findByText("You cannot add devices to any site.")).toBeTruthy();
  });
});
