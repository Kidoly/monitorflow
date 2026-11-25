import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { PsaCustomerLink } from "@/lib/api";
import { As, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { connection } from "@/test/psa";

import { PsaCustomerLinks } from "./PsaCustomerLinks";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });

function customer(overrides: Partial<PsaCustomerLink> = {}): PsaCustomerLink {
  return {
    customer_id: "c1",
    customer_name: "Example Industries SAS",
    client_id: null,
    client_name: null,
    linked_at: null,
    site_count: 2,
    matched_sites: 0,
    ...overrides,
  };
}

const list = (customers: PsaCustomerLink[]) => ({ status: 200, body: { customers } });
const suggestion = {
  customer_id: "c1",
  customer_name: "Example Industries SAS",
  client_id: 10,
  client_name: "Example Industries",
};

const page = (user = globalAdmin, onSaved = vi.fn()) =>
  render(
    <As user={user}>
      <PsaCustomerLinks connection={connection()} onSaved={onSaved} />
    </As>,
  );

describe("PsaCustomerLinks", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setActiveLocale("en");
  });

  it("suggests clients by name and confirms them all at once", async () => {
    const fetchMock = stubFetch({
      "GET /api/psa/customer-links": list([customer(), customer({ customer_id: "c2", customer_name: "Other" })]),
      "GET /api/psa/customer-links/suggestions": [
        { status: 200, body: { suggestions: [suggestion] } },
        { status: 200, body: { suggestions: [] } },
      ],
      "PUT /api/psa/customer-links/c1": { status: 200, body: customer({ client_id: 10, client_name: "Example Industries" }) },
    });
    const user = userEvent.setup();
    page();
    const table = await screen.findByRole("table", { name: "2. Match customers to HaloPSA clients" });
    expect(within(table).getAllByText("Not matched")).toHaveLength(2);
    expect(within(table).getAllByText("0 of 2")).toHaveLength(2);
    // HaloPSA is read only when asked.
    expect(fetchMock.mock.calls.some(([u]) => String(u).includes("suggestions"))).toBe(false);
    await user.click(screen.getByRole("button", { name: "Suggest matches by name" }));
    const suggestions = await screen.findByRole("table", { name: "Suggested matches" });
    expect(within(suggestions).getByText("Example Industries")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Confirm all (1)" }));
    const put = fetchMock.mock.calls.find(([u, init]) => u === "/api/psa/customer-links/c1" && init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({ client_id: 10 });
    expect(await screen.findByText("No HaloPSA client has the name of a customer without a match.")).toBeInTheDocument();
  });

  it("sets the default client of sites matched to nothing", async () => {
    const fetchMock = stubFetch({
      "GET /api/psa/customer-links": list([customer()]),
      "GET /api/psa/clients?search=": { status: 200, body: { records: [{ id: 11, name: "Example Logistics" }] } },
      "GET /api/psa/clients/11/sites": { status: 200, body: { records: [{ id: 110, name: "Head office" }] } },
      "PUT /api/psa/default-client": { status: 200, body: connection() },
    });
    const user = userEvent.setup();
    const onSaved = vi.fn();
    page(globalAdmin, onSaved);
    const group = await screen.findByRole("group", { name: "Sites matched to nothing:" });
    expect(group).toHaveTextContent("no ticket");
    await user.click(within(group).getByRole("button", { name: "Change" }));
    await user.click(await screen.findByRole("radio", { name: "Example Logistics" }));
    await screen.findByRole("option", { name: "Head office" });
    await user.selectOptions(screen.getByLabelText("Site in HaloPSA (optional)"), "110");
    await user.click(screen.getByRole("button", { name: "Save the match" }));
    const put = fetchMock.mock.calls.find(([u]) => u === "/api/psa/default-client");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({ client_id: 11, psa_site_id: 110 });
    expect(onSaved).toHaveBeenCalled();
  });

  it("asks a single company for one client, without customer wording", async () => {
    stubFetch({ "GET /api/psa/customer-links": list([customer()]) });
    page(single(globalAdmin));
    expect(await screen.findByRole("heading", { name: "2. HaloPSA client of this installation" })).toBeInTheDocument();
    expect(screen.queryByText(/customer/i)).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Suggest matches by name" })).not.toBeInTheDocument();
    expect(screen.queryByRole("group", { name: "Sites matched to nothing:" })).not.toBeInTheDocument();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ "GET /api/psa/customer-links": list([customer()]) });
    page();
    expect(await screen.findByRole("heading", { name: "2. Relier les clients aux clients HaloPSA" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Suggérer des correspondances par nom" })).toBeInTheDocument();
  });
});
