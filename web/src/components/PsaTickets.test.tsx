import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { PsaTicket } from "@/lib/api";
import { stubFetch } from "@/test/fetch";
import { connection, psaListStubs } from "@/test/psa";

import { PsaTickets } from "./PsaTickets";

vi.mock("next/navigation", () => import("@/test/navigation"));

function ticket(overrides: Partial<PsaTicket> = {}): PsaTicket {
  return {
    incident_id: "i1",
    ticket_id: 5001,
    state: "open",
    psa_status_id: null,
    checked_at: null,
    error: null,
    metric: "disk_used_percent",
    severity: "critical",
    server_name: "web-01",
    site_name: "paris",
    created_at: "2026-11-22T10:00:00Z",
    updated_at: "2026-11-22T10:00:00Z",
    closed_at: null,
    ...overrides,
  };
}

const page = (onSaved = vi.fn()) => render(<PsaTickets connection={connection()} onSaved={onSaved} />);

describe("PsaTickets", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setActiveLocale("en");
  });

  it("offers HaloPSA's lists and saves the choices", async () => {
    const fetchMock = stubFetch({
      ...psaListStubs,
      "GET /api/psa/tickets": { status: 200, body: { tickets: [] } },
      "PUT /api/psa/ticket-settings": { status: 200, body: connection() },
    });
    const user = userEvent.setup();
    const onSaved = vi.fn();
    page(onSaved);
    const type = await screen.findByLabelText("Ticket type");
    expect(screen.getByLabelText("When the incident is resolved")).toHaveValue("9");
    expect(screen.getByRole("radio", { name: "Critical incidents only" })).toBeChecked();
    await user.selectOptions(type, "1");
    await user.selectOptions(screen.getByLabelText(/^Category/), "Hardware>Server");
    await user.click(screen.getByRole("radio", { name: "Warning and critical incidents" }));
    await user.selectOptions(screen.getByLabelText("When the incident is resolved"), "");
    const critical = screen.getByLabelText(/^Priority of critical incidents/);
    await vi.waitFor(() => expect(critical).toBeEnabled());
    await user.selectOptions(critical, "1");
    await user.click(screen.getByRole("checkbox", { name: "New" }));
    await user.click(screen.getByRole("checkbox", { name: /assigned to a technician/ }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    const put = fetchMock.mock.calls.find(([u, init]) => u === "/api/psa/ticket-settings" && init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({
      ticket_type_id: 1,
      category: "Hardware>Server",
      min_severity: "warning",
      resolved_status_id: null,
      warning_priority_id: null,
      critical_priority_id: 1,
      closed_status_ids: [9, 1],
      acknowledge_on_assign: false,
    });
    expect(fetchMock.mock.calls.some(([u]) => u === "/api/psa/priorities?ticket_type_id=1")).toBe(true);
    expect(onSaved).toHaveBeenCalled();
    expect(screen.getByText("No ticket yet. They show here as incidents open.")).toBeInTheDocument();
  });

  it("lists the latest tickets with their state and HaloPSA's words", async () => {
    stubFetch({
      ...psaListStubs,
      "GET /api/psa/tickets": {
        status: 200,
        body: {
          tickets: [
            ticket(),
            ticket({ incident_id: "i2", ticket_id: null, state: "failed", error: "Veuillez renseigner la catégorie" }),
          ],
        },
      },
    });
    page();
    const table = await screen.findByRole("table", { name: "Latest tickets" });
    expect(within(table).getByText("#5001")).toBeInTheDocument();
    expect(within(table).getByText("Refused")).toBeInTheDocument();
    expect(within(table).getByText(/Veuillez renseigner/)).toBeInTheDocument();
  });

  it("shows HaloPSA's refusal of its lists", async () => {
    stubFetch({
      ...psaListStubs,
      "GET /api/psa/ticket-types": { status: 502, body: { error: "halo.example.com answered 403 Forbidden.", code: "psa_unavailable" } },
      "GET /api/psa/tickets": { status: 200, body: { tickets: [] } },
    });
    page();
    expect(await screen.findByRole("alert")).toHaveTextContent("403 Forbidden");
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ ...psaListStubs, "GET /api/psa/tickets": { status: 200, body: { tickets: [ticket()] } } });
    page();
    expect(await screen.findByLabelText("Type de ticket")).toBeInTheDocument();
    expect(await screen.findByText("Ouvert")).toBeInTheDocument();
  });
});
