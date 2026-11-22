import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { PsaSiteLink } from "@/lib/api";
import { As, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { PsaSiteLinks, matchedSummary } from "./PsaSiteLinks";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });

function site(overrides: Partial<PsaSiteLink> = {}): PsaSiteLink {
  return {
    site_id: "s1",
    site_name: "paris",
    customer_id: "c1",
    customer_name: "acme",
    client_id: null,
    client_name: null,
    psa_site_id: null,
    psa_site_name: null,
    linked_at: null,
    ...overrides,
  };
}

const matched = site({
  site_id: "s2",
  site_name: "lyon",
  client_id: 10,
  client_name: "Example Industries",
  psa_site_id: 100,
  psa_site_name: "Main office",
  linked_at: "2026-11-22T10:00:00Z",
});
const links = (sites: PsaSiteLink[]) => ({ status: 200, body: { sites } });
const clients = {
  status: 200,
  body: { records: [{ id: 10, name: "Example Industries" }, { id: 11, name: "Example Logistics" }] },
};

const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <PsaSiteLinks />
    </As>,
  );

describe("PsaSiteLinks", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setActiveLocale("en");
  });

  it("counts what is matched", () => {
    expect(matchedSummary([site(), matched])).toBe("Sites matched: 1 of 2");
  });

  it("lists sites with their client, customers only when there are several", async () => {
    stubFetch({ "GET /api/psa/site-links": links([site(), matched]) });
    page();
    const table = await screen.findByRole("table", { name: "Sites and their HaloPSA client" });
    expect(within(table).getByText("acme / paris")).toBeInTheDocument();
    expect(within(table).getByText("Not matched")).toBeInTheDocument();
    expect(within(table).getByText("Main office")).toBeInTheDocument();
    expect(screen.getByText("Sites matched: 1 of 2")).toBeInTheDocument();
  });

  it("searches HaloPSA's clients and saves the match with a site", async () => {
    const fetchMock = stubFetch({
      "GET /api/psa/site-links": [links([site()]), links([matched])],
      "GET /api/psa/clients?search=": clients,
      "GET /api/psa/clients?search=example": clients,
      "GET /api/psa/clients/10/sites": {
        status: 200,
        body: { records: [{ id: 100, name: "Main office" }, { id: 101, name: "Warehouse" }] },
      },
      "PUT /api/psa/site-links/s1": { status: 200, body: matched },
    });
    const user = userEvent.setup();
    page(single(globalAdmin));
    await user.click(await screen.findByRole("button", { name: "Match paris" }));
    await user.type(screen.getByLabelText("Search HaloPSA clients"), "example");
    await user.click(await screen.findByRole("radio", { name: "Example Industries" }));
    const select = screen.getByLabelText("Site in HaloPSA (optional)");
    await screen.findByRole("option", { name: "Warehouse" });
    await user.selectOptions(select, "101");
    await user.click(screen.getByRole("button", { name: "Save the match" }));
    const put = fetchMock.mock.calls.find(([u, init]) => u === "/api/psa/site-links/s1" && init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({ client_id: 10, psa_site_id: 101 });
    expect(await screen.findByText("Example Industries")).toBeInTheDocument();
  });

  it("shows HaloPSA's refusal", async () => {
    stubFetch({
      "GET /api/psa/site-links": links([site()]),
      "GET /api/psa/clients?search=": {
        status: 502,
        body: { error: "halo.example.com answered 403 Forbidden to the client list.", code: "psa_unavailable" },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Match paris" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("403 Forbidden");
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ "GET /api/psa/site-links": links([site(), matched]) });
    page();
    expect(await screen.findByText(/Sites reliés\s:\s1 sur 2/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Relier paris" })).toBeInTheDocument();
  });
});
