import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ApiKey, Customer, Site } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { ApiKeysPage, accessWords, reachOptions, reachWords } from "./ApiKeys";

const root = me({ role: "administrator", customer_id: null, site_id: null });

const KEY: ApiKey = {
  id: "k1",
  name: "HaloPSA",
  prefix: "abcd1234",
  role: "operator",
  customer_id: null,
  customer_name: null,
  site_id: null,
  site_name: null,
  areas: ["alerts"],
  created_by: "u1",
  created_by_username: "alice",
  created_at: "2026-11-01T10:00:00Z",
  expires_at: null,
  last_used_at: null,
};

function site(id: string, name: string, customerId: string, customerName: string): Site {
  return {
    id,
    name,
    customer_id: customerId,
    customer_name: customerName,
    server_count: 1,
    relay_count: 0,
    agent_auto_update: null,
  } as Site;
}

const ACME: Customer = { id: "c1", name: "acme", site_count: 1 };
const GLOBEX: Customer = { id: "c2", name: "globex", site_count: 1 };
const PARIS = site("s1", "paris", "c1", "acme");
const BERLIN = site("s2", "berlin", "c2", "globex");

afterEach(() => vi.unstubAllGlobals());

describe("words", () => {
  it("says what a key may do and reaches", () => {
    expect(accessWords(KEY)).toBe("Read and change · Alerts and checks");
    expect(accessWords({ ...KEY, role: "viewer", areas: null })).toBe("Read only · every area");
    expect(reachWords(KEY, false)).toBe("Everything");
    const siteKey = {
      ...KEY,
      customer_id: null,
      site_id: "s1",
      site_name: "paris",
      customer_name: "acme",
    };
    expect(reachWords(siteKey, false)).toBe("paris");
    expect(reachWords(siteKey, true)).toBe("acme › paris");
  });

  it("offers customers only once there are two", () => {
    expect(reachOptions(root, [ACME], [PARIS]).map((o) => o.label)).toEqual(["Everything", "paris"]);
    expect(reachOptions(root, [ACME, GLOBEX], [PARIS, BERLIN]).map((o) => o.label)).toEqual([
      "Everything",
      "acme",
      "globex",
      "acme › paris",
      "globex › berlin",
    ]);
    // A customer's administrator: only that customer and its sites.
    expect(reachOptions(customerAdmin, [ACME, GLOBEX], [PARIS, BERLIN]).map((o) => o.value)).toEqual(["c:c1", "s:s1"]);
  });
});

describe("ApiKeysPage", () => {
  it("makes a key limited to some areas and shows its secret once", async () => {
    const fetchMock = stubFetch({
      "GET /api/auth/me": { status: 200, body: root },
      "GET /api/api-keys": [
        { status: 200, body: { keys: [] } },
        { status: 200, body: { keys: [KEY] } },
      ],
      "GET /api/customers": { status: 200, body: { customers: [ACME] } },
      "GET /api/sites": { status: 200, body: { sites: [PARIS] } },
      "POST /api/api-keys": {
        status: 201,
        body: { key: KEY, secret: "mfk_secret" },
      },
    });
    render(
      <As user={root}>
        <ApiKeysPage />
      </As>,
    );
    expect(await screen.findByText("No API keys yet")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "New API key" }));
    await userEvent.type(screen.getByLabelText("What is it for?"), "HaloPSA");
    await userEvent.selectOptions(screen.getByLabelText(/^Access/), "operator");
    await userEvent.click(screen.getByLabelText("Only some areas"));
    await userEvent.click(screen.getByLabelText("Servers"));
    await userEvent.click(screen.getByRole("button", { name: "Make the key" }));

    expect(await screen.findByDisplayValue("mfk_secret")).toBeInTheDocument();
    expect(screen.getByText(/it is shown only once/)).toBeInTheDocument();
    const post = fetchMock.mock.calls.find((c) => c[1]?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      name: "HaloPSA",
      role: "operator",
      expires: "1y",
      areas: ["alerts"],
    });
    expect(await screen.findByText("mfk_abcd1234…")).toBeInTheDocument();
    expect(screen.getByText(/Read and change · Alerts and checks · Everything\. Made by alice\./)).toBeInTheDocument();
    // One customer: no reach to choose beyond everything and the site, no customer words.
    expect(screen.queryByText("acme")).not.toBeInTheDocument();
  });

  it("turns a key off", async () => {
    const fetchMock = stubFetch({
      "GET /api/api-keys": [
        { status: 200, body: { keys: [KEY] } },
        { status: 200, body: { keys: [] } },
      ],
      "GET /api/customers": { status: 200, body: { customers: [ACME] } },
      "GET /api/sites": { status: 200, body: { sites: [PARIS] } },
      "DELETE /api/api-keys/k1": { status: 204 },
    });
    render(
      <As user={root}>
        <ApiKeysPage />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Turn off HaloPSA" }));
    await userEvent.click(screen.getByRole("button", { name: "Turn off" }));
    expect(fetchMock.mock.calls.some((c) => String(c[0]) === "/api/api-keys/k1" && c[1]?.method === "DELETE")).toBe(
      true,
    );
    expect(await screen.findByText("No API keys yet")).toBeInTheDocument();
  });
});
