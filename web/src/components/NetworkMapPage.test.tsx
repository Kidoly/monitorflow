import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { MapLive, NetworkMap } from "@/lib/api";
import { emptyDocument } from "@/lib/maps";
import { As, me, oneSite } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { router } from "@/test/navigation";

import { NetworkMapView } from "./NetworkMapPage";
import { NetworkMapsPage } from "./NetworkMaps";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = oneSite(me({ role: "operator", customer_id: null, site_id: "site-1" }));
const viewer = oneSite(me({ role: "viewer", customer_id: null, site_id: "site-1" }));

const map: NetworkMap = {
  id: "m1",
  name: "Paris network",
  place: { kind: "site", id: "site-1", name: "paris" },
  updated_at: "2026-12-27T09:00:00Z",
  updated_by_username: "alice",
  background_updated_at: null,
  can_edit: true,
  document: emptyDocument(),
  revision: 4,
};

const node = (key: string, extra: Partial<MapLive["nodes"][number]>) => ({
  key,
  kind: "server" as const,
  name: key,
  site_id: "site-1",
  state: "ok" as const,
  server_id: null,
  relay_id: null,
  address: null,
  template: null,
  os_family: null,
  x: null,
  y: null,
  hidden: false,
  ...extra,
});

const live: MapLive = {
  id: "m1",
  name: "Paris network",
  group_by_site: true,
  sites: [{ id: "site-1", name: "paris" }],
  nodes: [
    node("relay:gw", { kind: "relay", name: "gw-1", relay_id: "gw", state: "offline" }),
    node("server:sw", { kind: "device", name: "sw-core", server_id: "sw", template: "switch", state: "critical" }),
    node("server:web", { name: "web-1", server_id: "web" }),
  ],
  links: [
    {
      id: "relay:gw|server:web", from: "relay:gw", to: "server:web", source: "relay", state: "critical",
      label: null, from_port: null, to_port: null, interface: null, traffic: null,
    },
    {
      id: "server:sw|server:web", from: "server:sw", to: "server:web", source: "neighbor", state: "ok",
      label: null, from_port: "Gi0/1", to_port: "eth0", interface: { server_id: "sw", instance: "Gi0/1" },
      traffic: { in_bytes_per_second: 1_250_000, out_bytes_per_second: null, utilization: 0.2, up: true },
    },
  ],
  zones: [],
  labels: [],
  background: null,
  neighbors_read_at: null,
  truncated: false,
  now: 0,
};

const put = (fetchMock: ReturnType<typeof stubFetch>) =>
  JSON.parse(String(fetchMock.mock.calls.find(([, init]) => init?.method === "PUT")?.[1]?.body));

/** Clicks a node of the map: a press and a release without moving. */
function press(key: string) {
  const element = document.querySelector(`[data-key="${key}"]`)!;
  fireEvent.pointerDown(element, { button: 0, pointerId: 1 });
  fireEvent.pointerUp(element.closest("svg")!, { pointerId: 1 });
}

describe("NetworkMapView", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    router.push.mockClear();
    window.history.replaceState(null, "", "/maps/view/?id=m1");
  });

  it("draws nodes and links in the colours of their states, and a click opens the node's page", async () => {
    stubFetch({
      "GET /api/maps/m1": { status: 200, body: { ...map, can_edit: false } },
      "GET /api/maps/m1/live": { status: 200, body: live },
    });
    render(
      <As user={viewer}>
        <NetworkMapView />
      </As>,
    );
    const canvas = await screen.findByRole("img", { name: "Network map Paris network" });
    expect(within(canvas).getByRole("link", { name: "web-1: OK" })).toBeTruthy();
    expect(within(canvas).getByRole("link", { name: "sw-core: Critical" })).toBeTruthy();
    expect(canvas.querySelector('[data-key="relay:gw"]')!.getAttribute("data-state")).toBe("offline");
    expect(canvas.querySelectorAll('line[data-state="critical"]')).toHaveLength(1);
    expect(within(canvas).getByText(/↓ 10/)).toBeTruthy();
    expect(screen.getByText(/1 critical, 0 warning, 1 down/)).toBeTruthy();
    // A viewer may not edit.
    expect(screen.queryByRole("button", { name: "Edit the map" })).toBeNull();
    press("server:web");
    expect(router.push).toHaveBeenCalledWith("/servers/view/?id=web");
  });

  it("edits: a zone, a label, a link drawn by hand and a node left off, saved with the revision", async () => {
    const fetchMock = stubFetch({
      "GET /api/maps/m1": { status: 200, body: map },
      "GET /api/maps/m1/live": { status: 200, body: live },
      "PUT /api/maps/m1": { status: 200, body: { ...map, revision: 5 } },
    });
    render(
      <As user={operator}>
        <NetworkMapView />
      </As>,
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Edit the map" }));
    await screen.findByRole("img", { name: "Network map Paris network" });
    await user.click(screen.getByRole("button", { name: "Add a zone" }));
    await user.click(screen.getByRole("button", { name: "Add a label" }));
    await user.click(screen.getByRole("button", { name: "Draw a link" }));
    expect(screen.getByText("Click the first node of the link.")).toBeTruthy();
    press("server:sw");
    press("relay:gw");
    expect(screen.getByRole("heading", { name: "Link" })).toBeTruthy();
    await user.type(screen.getByLabelText("Label", { selector: "input" }), "Fibre");
    press("server:web");
    await user.click(screen.getByRole("button", { name: "Leave off the map" }));
    expect(screen.getByText("1 node left off")).toBeTruthy();
    await user.click(screen.getByRole("switch", { name: "Addresses found by network scans" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(put(fetchMock)).toBeTruthy());
    const body = put(fetchMock);
    expect(body.revision).toBe(4);
    expect(body.place).toEqual({ kind: "site", id: "site-1" });
    expect(body.document.zones).toHaveLength(1);
    expect(body.document.labels[0].text).toBe("New label");
    expect(body.document.links).toEqual([{ id: "link1", from: "server:sw", to: "relay:gw", label: "Fibre" }]);
    expect(body.document.nodes.find((n: { key: string }) => n.key === "server:web").hidden).toBe(true);
    expect(body.document.sources.discovered).toBe(true);
  });

  it("says when someone saved in between", async () => {
    stubFetch({
      "GET /api/maps/m1": { status: 200, body: map },
      "GET /api/maps/m1/live": { status: 200, body: live },
      "PUT /api/maps/m1": { status: 409, body: { error: "Someone changed this map since you opened it." } },
    });
    window.history.replaceState(null, "", "/maps/view/?id=m1&edit=1");
    render(
      <As user={operator}>
        <NetworkMapView />
      </As>,
    );
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Save" }));
    expect((await screen.findByRole("alert")).textContent).toMatch(/The map was not saved.*Someone changed/);
  });
});

describe("NetworkMapsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("lists maps and makes one of a site, opened for editing", async () => {
    const fetchMock = stubFetch({
      "GET /api/maps": { status: 200, body: { maps: [map] } },
      "GET /api/sites": { status: 200, body: { sites: [{ id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme" }] } },
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] } },
      "POST /api/maps": { status: 201, body: { ...map, id: "m2" } },
    });
    render(
      <As user={operator}>
        <NetworkMapsPage />
      </As>,
    );
    expect(await screen.findByRole("link", { name: "Paris network" })).toBeTruthy();
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "New map" }));
    await user.click(screen.getByRole("button", { name: "Make the map" }));
    expect(screen.getByRole("alert").textContent).toBe("Give the map a name.");
    await user.type(screen.getByLabelText("Name"), "Rack A");
    await user.click(screen.getByRole("button", { name: "Make the map" }));
    await waitFor(() => expect(router.push).toHaveBeenCalledWith("/maps/view/?id=m2&edit=1"));
    const sent = JSON.parse(String(fetchMock.mock.calls.find(([, init]) => init?.method === "POST")?.[1]?.body));
    expect(sent.name).toBe("Rack A");
    expect(sent.place).toEqual({ kind: "site", id: "site-1" });
  });
});
