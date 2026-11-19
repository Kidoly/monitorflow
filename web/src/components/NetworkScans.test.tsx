import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, me, oneSite } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { router } from "@/test/navigation";

import { NetworkScansPage } from "./NetworkScans";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = oneSite(me({ role: "operator", customer_id: null, site_id: "site-1" }));
const viewer = oneSite(me({ role: "viewer", customer_id: null, site_id: "site-1" }));
const site = { id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme" };
const probes = {
  probes: [{ probe: "central", relay_id: null, key: "central", name: null, online: true, default: true }],
};
const scan = {
  id: "s1", site_id: "site-1", site_name: "paris", name: "Office", targets: ["192.0.2.0/24"], addresses: 254,
  ports: [22, 80], probe: "central", probe_relay_id: null, probe_key: "central", every: "day", notify: false,
  snmp: null, created_at: "2026-11-18T10:00:00Z", updated_at: "2026-11-18T10:00:00Z",
  last_run: { id: "r1", state: "done", trigger: "person", queued_at: "2026-11-18T10:00:00Z", started_at: null,
              finished_at: "2026-11-18T10:02:00Z", addresses: 254, scanned: 254, answered: 7, compared: false,
              new: 0, gone: [], dropped: 0, ping_error: null, error: null },
};
const sent = (fetchMock: ReturnType<typeof stubFetch>) =>
  JSON.parse(String(fetchMock.mock.calls.find(([, init]) => init?.method === "POST")?.[1]?.body));

describe("NetworkScansPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("lists scans and creates one that starts at once", async () => {
    const fetchMock = stubFetch({
      "GET /api/network-scans": { status: 200, body: { scans: [scan] } },
      "GET /api/sites": { status: 200, body: { sites: [site] } },
      "GET /api/sites/site-1/probes": { status: 200, body: probes },
      "POST /api/network-scans": { status: 201, body: { ...scan, id: "s2" } },
    });
    render(
      <As user={operator}>
        <NetworkScansPage />
      </As>,
    );
    expect(await screen.findByRole("link", { name: "Office" })).toBeTruthy();
    expect(screen.getByText("Every day")).toBeTruthy();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "New scan" }));
    await user.click(screen.getByRole("button", { name: "Scan now" }));
    expect(screen.getByText("Give it a name.")).toBeTruthy();
    expect(screen.getByText("Enter at least one IP address, block or range.")).toBeTruthy();
    await user.type(screen.getByLabelText(/^Name/), "Lab");
    await user.type(screen.getByLabelText(/^Addresses/), "printer.lan");
    await user.click(screen.getByRole("button", { name: "Scan now" }));
    expect(screen.getByText("printer.lan is not an IP address, block or range.")).toBeTruthy();
    await user.clear(screen.getByLabelText(/^Addresses/));
    await user.type(screen.getByLabelText(/^Addresses/), "192.0.2.0/28, 198.51.100.5");
    await user.clear(screen.getByLabelText(/^Ports/));
    await user.type(screen.getByLabelText(/^Ports/), "22, 443");
    await waitFor(() => expect((screen.getByLabelText("Scanned from") as HTMLSelectElement).value).toBe("central"));
    await user.click(screen.getByRole("button", { name: "Scan now" }));
    await waitFor(() => expect(router.push).toHaveBeenCalledWith("/discovery/view/?id=s2"));
    expect(sent(fetchMock)).toEqual({
      site_id: "site-1", name: "Lab", targets: ["192.0.2.0/28", "198.51.100.5"], ports: [22, 443],
      every: null, notify: false, probe: "central",
    });
  });

  it("offers nothing to change to a viewer", async () => {
    stubFetch({
      "GET /api/network-scans": { status: 200, body: { scans: [] } },
      "GET /api/sites": { status: 200, body: { sites: [site] } },
    });
    render(
      <As user={viewer}>
        <NetworkScansPage />
      </As>,
    );
    expect(await screen.findByText("No scan yet")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "New scan" })).toBeNull();
  });
});
