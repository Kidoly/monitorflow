import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { ServerDetail, SnmpCheck } from "@/lib/api";
import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { SnmpChecks } from "./SnmpChecks";

vi.mock("next/navigation", () => import("@/test/navigation"));

const device = {
  id: "d1",
  peer_id: "fw-edge",
  hostname: "fw-edge",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  kind: "device",
  address: "192.0.2.20",
  revoked_at: null,
  created_at: "2026-11-19T10:00:00Z",
} as unknown as ServerDetail;

const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });

const check: SnmpCheck = {
  entry_id: "e1",
  name: "snmp_fw",
  instance: "central",
  probe: "central",
  probe_relay_id: null,
  interval_ms: 60_000,
  enabled: true,
  table: null,
  name_column: null,
  channels: [
    {
      oid: "1.3.6.1.4.1.12356.101.4.1.8.0",
      name: "Sessions",
      unit: "sessions",
      limits: { comparison: ">", warning: 5000, critical: 8000, duration_s: 300 },
      key: "sessions",
      metric: "monitorflow_snmp_fw_sessions",
      state_metric: null,
      values: [{ instance: "", value: 1532, state: null, text: null, at: "2026-11-19T10:00:00Z" }],
    },
    {
      oid: "1.3.6.1.4.1.12356.101.13.2.1.1.15.1",
      name: "HA state",
      lookup: "builtin:ok_warning_failed",
      key: "ha",
      metric: "monitorflow_snmp_fw_ha",
      state_metric: "monitorflow_snmp_fw_ha_state",
      values: [{ instance: "", value: 3, state: 2, text: "Failed", at: "2026-11-19T10:00:00Z" }],
    },
  ],
};

const CHECKS = "GET /api/devices/d1/snmp-checks";
const LOOKUPS = { status: 200, body: { lookups: [{ id: "builtin:ok_warning_failed", builtin: true, customer_id: null, name: "OK, warning, failed", entries: [], otherwise: "warning", used_by: 1, updated_at: null, can_change: false }] } };
const PROBES = { status: 200, body: { probes: [{ probe: "central", relay_id: null, key: "central", name: null, online: true, default: true }] } };

afterEach(() => setActiveLocale("en"));

describe("SnmpChecks", () => {
  it("shows each channel's value, state and limits, and only operators change them", async () => {
    stubFetch({ [CHECKS]: { status: 200, body: { checks: [check] } } });
    const { unmount } = render(
      <As user={viewer}>
        <SnmpChecks server={device} />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "snmp_fw" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent("Sessions");
    expect(rows[0]).toHaveTextContent("1,532 sessions");
    expect(rows[0]).toHaveTextContent("Warn above 5000, critical above 8000");
    expect(rows[1]).toHaveTextContent("Critical");
    expect(rows[1]).toHaveTextContent("Failed");
    expect(rows[1]).toHaveTextContent("Alerts on its state");
    expect(screen.getByText(/Checked from the central server/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Add a custom check" })).toBeNull();
    unmount();

    setActiveLocale("fr");
    stubFetch({ [CHECKS]: { status: 200, body: { checks: [check] } } });
    render(
      <As user={operator}>
        <SnmpChecks server={device} />
      </As>,
    );
    expect(await screen.findByRole("button", { name: "Ajouter un contrôle personnalisé" })).toBeInTheDocument();
    expect(screen.getByRole("table", { name: "snmp_fw" })).toHaveTextContent("Critique");
  });

  it("adds a check after a test, with a channel from Browse", async () => {
    const fetchMock = stubFetch({
      [CHECKS]: { status: 200, body: { checks: [] } },
      "GET /api/snmp-lookups": LOOKUPS,
      "GET /api/sites/site-1/probes": PROBES,
      "POST /api/devices/d1/snmp/read": {
        status: 200,
        body: { probe: "central", cut: false, objects: [{ oid: "1.3.6.1.4.1.12356.101.4.1.1.0", name: "fgSysVersion", type: "OctetString", value: "v7.2.5" }] },
      },
      "POST /api/devices/d1/snmp/test": {
        status: 200,
        body: {
          probe: "central",
          cut: false,
          rows: [
            {
              instance: "",
              channels: [
                { key: "sessions", name: "Sessions", oid: "1.3.6.1.4.1.12356.101.4.1.8.0", type: "Gauge32", raw: "1532", value: 1532, state: null, text: null, note: null },
                { key: "fgsysversion", name: "fgSysVersion", oid: "1.3.6.1.4.1.12356.101.4.1.1.0", type: "OctetString", raw: "v7.2.5", value: null, state: 0, text: "v7.2.5", note: null },
              ],
            },
          ],
        },
      },
      "POST /api/devices/d1/snmp-checks": { status: 201, body: check },
    });
    render(
      <As user={operator}>
        <SnmpChecks server={device} />
      </As>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Add a custom check" }));
    const form = screen.getByRole("form", { name: "Add a custom SNMP check" });
    fireEvent.change(within(form).getByLabelText(/^Name\s*Lowercase/), { target: { value: "snmp_fw" } });
    const first = within(form).getByRole("group", { name: "Channel 1" });
    fireEvent.change(within(first).getByLabelText(/^OID/), { target: { value: "1.3.6.1.4.1.12356.101.4.1.8.0" } });
    fireEvent.change(within(first).getByLabelText("Name"), { target: { value: "Sessions" } });
    fireEvent.change(within(first).getByLabelText("Unit"), { target: { value: "sessions" } });
    fireEvent.change(within(first).getByLabelText("Alert when the value is"), { target: { value: ">" } });
    fireEvent.change(within(first).getByLabelText("Warning at"), { target: { value: "5000" } });

    fireEvent.click(within(form).getByRole("button", { name: "Browse the device…" }));
    const browse = within(form).getByRole("group", { name: "Browse fw-edge" });
    fireEvent.click(within(browse).getByRole("button", { name: "List" }));
    fireEvent.click(await within(browse).findByRole("button", { name: "Watch fgSysVersion" }));
    const second = within(form).getByRole("group", { name: "Channel 2" });
    expect(within(second).getByLabelText("Kind")).toHaveValue("text");

    fireEvent.click(within(form).getByRole("button", { name: "Test" }));
    const tested = await screen.findByRole("table", { name: "What the central server read" });
    expect(tested).toHaveTextContent("Gauge32 1532");
    expect(tested).toHaveTextContent("1,532 sessions");
    expect(tested).toHaveTextContent("OK · v7.2.5");

    fireEvent.click(within(form).getByRole("button", { name: "Save the check" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([url, init]) => url === "/api/devices/d1/snmp-checks" && init?.method === "POST")).toBe(true));
    const [, init] = fetchMock.mock.calls.find(([url, i]) => url === "/api/devices/d1/snmp-checks" && i?.method === "POST")!;
    const body = JSON.parse(String(init?.body));
    expect(body).toMatchObject({
      name: "snmp_fw",
      probe: "central",
      interval_ms: 60000,
      channels: [
        { oid: "1.3.6.1.4.1.12356.101.4.1.8.0", name: "Sessions", unit: "sessions", limits: { comparison: ">", warning: 5000, critical: null, duration_s: 0 } },
        { oid: "1.3.6.1.4.1.12356.101.4.1.1.0", name: "fgSysVersion", type: "text", limits: null },
      ],
    });
  });

  it("says what is wrong before sending", async () => {
    const fetchMock = stubFetch({
      [CHECKS]: { status: 200, body: { checks: [] } },
      "GET /api/snmp-lookups": LOOKUPS,
      "GET /api/sites/site-1/probes": PROBES,
    });
    render(
      <As user={operator}>
        <SnmpChecks server={device} />
      </As>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Add a custom check" }));
    fireEvent.click(screen.getByRole("button", { name: "Save the check" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Channel 1: enter a numeric OID");
    expect(fetchMock.mock.calls.every(([, init]) => (init?.method ?? "GET") === "GET")).toBe(true);
  });
});
