import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { SnmpLookup } from "@/lib/api";
import { As, customerAdmin, single, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { SnmpLibraryPage, entriesOf, entriesText } from "./SnmpLibrary";

vi.mock("next/navigation", () => import("@/test/navigation"));

const builtin: SnmpLookup = {
  id: "builtin:truth_value",
  builtin: true,
  customer_id: null,
  name: "True or false (TruthValue)",
  entries: [
    { value: 1, name: "True", state: "ok" },
    { value: 2, name: "False", state: "critical" },
  ],
  otherwise: "warning",
  used_by: 0,
  updated_at: null,
  can_change: false,
};

const raid: SnmpLookup = {
  ...builtin,
  id: "l1",
  builtin: false,
  customer_id: "c1",
  name: "RAID state",
  entries: [
    { value: 1, name: "Normal", state: "ok" },
    { value: 10, max: 19, name: "Degraded", state: "critical" },
  ],
  used_by: 2,
  can_change: true,
};

const admin = single(customerAdmin);

afterEach(() => setActiveLocale("en"));

describe("SnmpLibraryPage", () => {
  it("lists lookups and MIBs, built-in ones without buttons", async () => {
    stubFetch({
      "GET /api/snmp-lookups": { status: 200, body: { lookups: [builtin, raid] } },
      "GET /api/snmp-mibs": {
        status: 200,
        body: { mibs: [{ id: "m1", customer_id: "c1", module: "TEST-NAS-MIB", file_name: "nas.txt", objects: 14, uploaded_at: "2026-11-19T10:00:00Z", can_change: true }] },
      },
    });
    render(
      <As user={admin}>
        <SnmpLibraryPage />
      </As>,
    );
    const lookups = await screen.findByRole("table", { name: "Lookups" });
    const rows = within(lookups).getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent("True or false (TruthValue)");
    expect(rows[0]).toHaveTextContent("Built in");
    expect(within(rows[0]!).queryByRole("button")).toBeNull();
    expect(rows[1]).toHaveTextContent("1 = Normal, 10..19 = Degraded");
    expect(rows[1]).toHaveTextContent("2 checks");
    expect(within(rows[1]!).getByRole("button", { name: "Change RAID state" })).toBeInTheDocument();
    const mibs = await screen.findByRole("table", { name: "MIB files" });
    expect(mibs).toHaveTextContent("TEST-NAS-MIB");
    expect(mibs).toHaveTextContent("14 objects");
  });

  it("uploads files and makes a lookup of what they name", async () => {
    const fetchMock = stubFetch({
      "GET /api/snmp-lookups": { status: 200, body: { lookups: [builtin] } },
      "GET /api/snmp-mibs": { status: 200, body: { mibs: [] } },
      "POST /api/snmp-mibs": {
        status: 201,
        body: {
          mibs: [{ id: "m1", customer_id: "c1", module: "TEST-NAS-MIB", file_name: "nas.txt", objects: 3, uploaded_at: "2026-11-19T10:00:00Z", can_change: true }],
          offered_lookups: [{ name: "raidStatus", module: "TEST-NAS-MIB", entries: [{ value: 1, name: "normal", state: "ok" }, { value: 12, name: "crashed", state: "critical" }] }],
        },
      },
      "POST /api/snmp-lookups": { status: 201, body: raid },
    });
    render(
      <As user={admin}>
        <SnmpLibraryPage />
      </As>,
    );
    const input = await screen.findByLabelText(/Upload MIB files/);
    const file = new File(["TEST-NAS-MIB DEFINITIONS ::= BEGIN END"], "nas.txt", { type: "text/plain" });
    fireEvent.change(input, { target: { files: [file] } });
    expect(await screen.findByText("Added: TEST-NAS-MIB.")).toBeInTheDocument();
    const upload = fetchMock.mock.calls.find(([url, init]) => url === "/api/snmp-mibs" && init?.method === "POST")!;
    expect(JSON.parse(String(upload[1]?.body))).toEqual({ files: [{ name: "nas.txt", text: "TEST-NAS-MIB DEFINITIONS ::= BEGIN END" }] });

    fireEvent.click(screen.getByRole("button", { name: "Make a lookup of raidStatus" }));
    const form = screen.getByRole("form", { name: "New lookup" });
    expect(within(form).getByLabelText("Name")).toHaveValue("raidStatus");
    fireEvent.click(within(form).getByRole("button", { name: "Save the lookup" }));
    await waitFor(() => expect(fetchMock.mock.calls.some(([url, init]) => url === "/api/snmp-lookups" && init?.method === "POST")).toBe(true));
    const made = fetchMock.mock.calls.find(([url, init]) => url === "/api/snmp-lookups" && init?.method === "POST")!;
    expect(JSON.parse(String(made[1]?.body))).toEqual({
      name: "raidStatus",
      entries: [
        { value: 1, name: "normal", state: "ok" },
        { value: 12, name: "crashed", state: "critical" },
      ],
      otherwise: "warning",
    });
  });

  it("is for administrators, and speaks French", async () => {
    stubFetch({ "GET /api/snmp-lookups": { status: 200, body: { lookups: [] } }, "GET /api/snmp-mibs": { status: 200, body: { mibs: [] } } });
    const { unmount } = render(
      <As user={viewer}>
        <SnmpLibraryPage />
      </As>,
    );
    expect(screen.getByText("Lookups and MIBs are for administrators")).toBeInTheDocument();
    unmount();
    setActiveLocale("fr");
    render(
      <As user={admin}>
        <SnmpLibraryPage />
      </As>,
    );
    expect(await screen.findByRole("heading", { name: "Tables de correspondance et MIB SNMP" })).toBeInTheDocument();
  });
});

describe("lookup entries", () => {
  it("read back what the form holds, or the first one that does not", () => {
    const entry = (patch: object) => ({ kind: "value" as const, value: "1", max: "", text: "", name: "OK", state: "ok" as const, ...patch });
    expect(entriesOf([entry({}), entry({ kind: "range", value: "10", max: "19", name: "Degraded" }), entry({ kind: "text", text: "on", name: "On" })])).toEqual([
      { value: 1, name: "OK", state: "ok" },
      { value: 10, max: 19, name: "Degraded", state: "ok" },
      { text: "on", name: "On", state: "ok" },
    ]);
    expect(entriesOf([entry({}), entry({ value: "1.5" })])).toBe(2);
    expect(entriesOf([entry({ kind: "range", value: "5", max: "1" })])).toBe(1);
    expect(entriesOf([entry({ name: " " })])).toBe(1);
    expect(entriesText([{ text: "on", name: "On", state: "ok" }])).toBe("“on” = On");
  });
});
