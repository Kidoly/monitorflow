import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { IntegrationEndpoint } from "@/lib/api";
import { EVENT_GROUPS } from "@/lib/integrations";
import { As, customerAdmin, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { IntegrationsPage } from "./IntegrationsAdmin";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({
  role: "administrator",
  customer_id: null,
  site_id: null,
});
const ENDPOINTS = "/api/integrations";
const EVENTS = EVENT_GROUPS.flatMap((g) => g.events.map((e) => e.event));
const oneCustomer = {
  status: 200,
  body: { customers: [{ id: "c1", name: "default", site_count: 1 }] },
};
const twoCustomers = {
  status: 200,
  body: {
    customers: [
      { id: "c1", name: "acme", site_count: 1 },
      { id: "c2", name: "globex", site_count: 1 },
    ],
  },
};
const noSites = { status: 200, body: { sites: [] } };

function endpoint(
  overrides: Partial<IntegrationEndpoint> = {},
): IntegrationEndpoint {
  return {
    id: "i1",
    name: "HaloPSA",
    origin: "https://psa.example",
    events: EVENTS,
    customer_id: null,
    customer_name: null,
    site_id: null,
    site_name: null,
    allow_private: false,
    state: "on",
    created_at: "2026-11-07T10:00:00Z",
    failing_since: null,
    last_success_at: null,
    pending: 0,
    last_error: null,
    ...overrides,
  };
}

const list = (endpoints: IntegrationEndpoint[]) => ({
  status: 200,
  body: { endpoints, events: EVENTS },
});

const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <IntegrationsPage />
    </As>,
  );

function sent(
  fetchMock: ReturnType<typeof stubFetch>,
  method: string,
  url: string,
) {
  const call = fetchMock.mock.calls.find(
    ([u, init]) => u === url && init?.method === method,
  );
  return JSON.parse(String(call?.[1]?.body));
}

describe("IntegrationsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("is for administrators", () => {
    page(viewer);
    expect(
      screen.getByText("Integrations need an administrator"),
    ).toBeInTheDocument();
  });

  it("adds an integration and shows its secret once", async () => {
    const created = endpoint();
    const fetchMock = stubFetch({
      [`GET ${ENDPOINTS}`]: [list([]), list([created])],
      "GET /api/customers": oneCustomer,
      "GET /api/sites": noSites,
      [`POST ${ENDPOINTS}`]: {
        status: 201,
        body: { endpoint: created, secret: "mfwh_abc" },
      },
      [`POST ${ENDPOINTS}/i1/test`]: {
        status: 200,
        body: { ok: true, status: 204, error: null },
      },
    });
    const user = userEvent.setup();
    page();
    expect(await screen.findByText("No integrations yet")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add integration" }));
    // One customer: no customer choice, and every event by default.
    expect(
      screen.queryByLabelText("Send events about"),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("checkbox", { name: "All events" })).toBeChecked();
    await user.type(screen.getByLabelText("Name"), "HaloPSA");
    await user.type(
      screen.getByLabelText(/^URL/),
      "https://psa.example/hooks/monitorflow",
    );
    await user.click(screen.getByRole("checkbox", { name: "Acknowledged" }));
    await user.click(screen.getByRole("button", { name: "Save" }));

    expect(await screen.findByDisplayValue("mfwh_abc")).toBeInTheDocument();
    expect(screen.getByText(/it is not shown again/)).toBeInTheDocument();
    expect(sent(fetchMock, "POST", ENDPOINTS)).toEqual({
      name: "HaloPSA",
      url: "https://psa.example/hooks/monitorflow",
      events: EVENTS.filter((e) => e !== "incident.acknowledged"),
      customer_id: null,
      site_id: null,
      allow_private: false,
    });
    const panel = screen.getByRole("region", { name: "Signing secret" });
    await user.click(
      within(panel).getByRole("button", { name: "Send test event" }),
    );
    expect(await within(panel).findByText("Test accepted")).toBeInTheDocument();
    await user.click(within(panel).getByRole("button", { name: "Done" }));
    expect(screen.queryByDisplayValue("mfwh_abc")).not.toBeInTheDocument();
  });

  it("never shows the stored URL's path and keeps it when left empty", async () => {
    const fetchMock = stubFetch({
      [`GET ${ENDPOINTS}`]: list([
        endpoint({ allow_private: true, customer_id: "c1", customer_name: "acme" }),
      ]),
      "GET /api/customers": oneCustomer,
      "GET /api/sites": noSites,
      [`PUT ${ENDPOINTS}/i1`]: {
        status: 200,
        body: { endpoint: endpoint(), secret: null },
      },
    });
    const user = userEvent.setup();
    page(customerAdmin);
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const url = screen.getByLabelText(/^URL/);
    expect(url).toHaveValue("");
    expect(url).not.toBeRequired();
    expect(url).toHaveAttribute(
      "placeholder",
      "https://psa.example/…, saved; leave empty to keep it",
    );
    await user.click(screen.getByRole("button", { name: "Save" }));
    // The same URL keeps a global administrator's private address choice.
    expect(sent(fetchMock, "PUT", `${ENDPOINTS}/i1`)).toMatchObject({
      url: "",
      allow_private: true,
    });
  });

  it("keeps the secret for the same URL and shows a new one for a new URL", async () => {
    const moved = endpoint({ origin: "https://psa2.example" });
    const fetchMock = stubFetch({
      [`GET ${ENDPOINTS}`]: [
        list([endpoint()]),
        list([endpoint({ name: "Halo" })]),
        list([moved]),
      ],
      "GET /api/customers": oneCustomer,
      "GET /api/sites": noSites,
      [`PUT ${ENDPOINTS}/i1`]: [
        {
          status: 200,
          body: { endpoint: endpoint({ name: "Halo" }), secret: null },
        },
        { status: 200, body: { endpoint: moved, secret: "mfwh_new" } },
      ],
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const name = screen.getByLabelText("Name");
    await user.clear(name);
    await user.type(name, "Halo");
    expect(
      screen.queryByText(/A new URL gets a new signing secret/),
    ).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByText("Halo")).toBeInTheDocument();
    expect(
      screen.queryByRole("region", { name: "Signing secret" }),
    ).not.toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Edit" }));
    const url = screen.getByLabelText(/^URL/);
    await user.type(url, "https://psa2.example/hooks");
    expect(
      screen.getByText(/A new URL gets a new signing secret/),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByDisplayValue("mfwh_new")).toBeInTheDocument();
    const puts = fetchMock.mock.calls.filter(
      ([, init]) => init?.method === "PUT",
    );
    expect(puts.map(([, init]) => JSON.parse(String(init?.body)).url)).toEqual([
      "",
      "https://psa2.example/hooks",
    ]);
  });

  it("says when an integration was turned off and turns it back on", async () => {
    const off = endpoint({
      state: "disabled",
      failing_since: "2026-11-06T10:00:00Z",
      pending: 3,
    });
    const fetchMock = stubFetch({
      [`GET ${ENDPOINTS}`]: [list([off]), list([endpoint()])],
      "GET /api/customers": oneCustomer,
      "GET /api/sites": noSites,
      [`POST ${ENDPOINTS}/i1/state`]: { status: 200, body: endpoint() },
    });
    const user = userEvent.setup();
    page();
    expect(await screen.findByText("Turned off")).toBeInTheDocument();
    expect(
      screen.getByText(/Nothing reached it for a day/),
    ).toBeInTheDocument();
    expect(screen.getByText(/3 waiting/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Turn on" }));
    expect(sent(fetchMock, "POST", `${ENDPOINTS}/i1/state`)).toEqual({
      state: "on",
    });
  });

  it("shows deliveries and sends one again", async () => {
    const fetchMock = stubFetch({
      [`GET ${ENDPOINTS}`]: list([
        endpoint({ last_success_at: "2026-11-07T10:00:00Z" }),
      ]),
      "GET /api/customers": oneCustomer,
      "GET /api/sites": noSites,
      [`GET ${ENDPOINTS}/i1/deliveries`]: {
        status: 200,
        body: {
          deliveries: [
            {
              id: 7,
              event_id: "e7",
              event: "server.added",
              occurred_at: "2026-11-07T10:00:00Z",
              state: "failed",
              replay: false,
              attempts: 30,
              last_status: 500,
              last_error: "psa.example answered 500 Internal Server Error.",
              last_attempt_at: "2026-11-07T10:00:00Z",
              next_at: null,
              finished_at: "2026-11-07T10:00:00Z",
            },
          ],
          next_before: null,
        },
      },
      [`POST ${ENDPOINTS}/i1/replay`]: { status: 200, body: { queued: 1 } },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Deliveries" }));
    expect(
      await screen.findByText("Gave up after 30 tries (500)"),
    ).toBeInTheDocument();
    expect(screen.getByText("Servers: Added")).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Send Servers: Added again" }),
    );
    expect(
      await screen.findByText(/1 event will be sent again, with the same IDs/),
    ).toBeInTheDocument();
    expect(sent(fetchMock, "POST", `${ENDPOINTS}/i1/replay`)).toEqual({
      event_id: "e7",
    });
  });

  it("lets a customer administrator choose among their places but not private addresses", async () => {
    stubFetch({
      [`GET ${ENDPOINTS}`]: list([
        endpoint({ customer_id: "c1", customer_name: "acme" }),
      ]),
      "GET /api/customers": twoCustomers,
      "GET /api/sites": noSites,
    });
    const user = userEvent.setup();
    page(customerAdmin);
    expect(await screen.findByText("acme")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add integration" }));
    expect(
      screen.queryByRole("checkbox", { name: /Allow private addresses/ }),
    ).not.toBeInTheDocument();
  });

  it("never speaks of customers to someone who sees one (ADR-0104)", async () => {
    const single = {
      ...me({ role: "administrator", customer_id: "c1", site_id: null }),
      multi_customer: false,
    };
    stubFetch({
      [`GET ${ENDPOINTS}`]: list([
        endpoint({ customer_id: "c1", customer_name: "acme" }),
        endpoint({
          id: "i2",
          name: "Paris PSA",
          customer_id: "c1",
          customer_name: "acme",
          site_id: "site-1",
          site_name: "paris",
        }),
      ]),
      "GET /api/customers": {
        status: 200,
        body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] },
      },
      "GET /api/sites": {
        status: 200,
        body: {
          sites: [
            {
              id: "site-1",
              name: "paris",
              customer_id: "c1",
              customer_name: "acme",
            },
          ],
        },
      },
    });
    const user = userEvent.setup();
    page(single);
    const table = await screen.findByRole("table", { name: "Integrations" });
    expect(within(table).getByText("All sites")).toBeInTheDocument();
    expect(within(table).getByText("paris")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Add integration" }));
    const picker = screen.getByLabelText(/^Send events about/);
    expect(
      within(picker)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["All sites", "paris"]);
    expect(screen.getByText("Events of all your sites.")).toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/customer|acme/i);
  });

  it("offers private addresses to a global administrator", async () => {
    stubFetch({
      [`GET ${ENDPOINTS}`]: list([]),
      "GET /api/customers": oneCustomer,
      "GET /api/sites": noSites,
    });
    const user = userEvent.setup();
    page();
    await user.click(
      await screen.findByRole("button", { name: "Add integration" }),
    );
    expect(
      screen.getByRole("checkbox", { name: /Allow private addresses/ }),
    ).not.toBeChecked();
  });
});
