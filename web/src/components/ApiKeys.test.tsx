import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { ApiKey, Customer, Site } from "@/lib/api";
import { As, customerAdmin, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { ApiKeysPage, accessWords, reachOptions, reachWords } from "./ApiKeys";

const root = me({ role: "administrator", customer_id: null, site_id: null });

const KEY: ApiKey = {
  id: "k1",
  name: "Ticketing",
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

  it("speaks French", () => {
    setActiveLocale("fr");
    expect(accessWords(KEY)).toBe("Lecture et modification · Alertes et contrôles");
    expect(accessWords({ ...KEY, role: "viewer", areas: null })).toBe("Lecture seule · tous les domaines");
    expect(reachWords(KEY, false)).toBe("Tout");
  });

  it("offers customers only to someone who sees several", () => {
    expect(reachOptions(root, false, [ACME], [PARIS]).map((o) => o.label)).toEqual(["Everything", "paris"]);
    expect(reachOptions(root, true, [ACME, GLOBEX], [PARIS, BERLIN]).map((o) => o.label)).toEqual([
      "Everything",
      "acme",
      "globex",
      "acme › paris",
      "globex › berlin",
    ]);
    // A customer's administrator: only that customer and its sites.
    expect(reachOptions(customerAdmin, true, [ACME, GLOBEX], [PARIS, BERLIN]).map((o) => o.value)).toEqual([
      "c:c1",
      "s:s1",
    ]);
  });
});

describe("ApiKeysPage", () => {
  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({
      "GET /api/api-keys": { status: 200, body: { keys: [KEY] } },
      "GET /api/customers": { status: 200, body: { customers: [ACME] } },
      "GET /api/sites": { status: 200, body: { sites: [PARIS] } },
    });
    render(
      <As user={single(root)}>
        <ApiKeysPage />
      </As>,
    );
    expect(await screen.findByText(/Lecture et modification · Alertes et contrôles · Tout\. Créée par alice\./)).toBeInTheDocument();
    expect(screen.getByText(/N’expire jamais\. Pas encore utilisée\./)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Nouvelle clé d’API" }));
    expect(screen.getByLabelText(/^À quoi sert-elle/)).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "Dans 90 jours" })).toBeInTheDocument();
  });

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
      <As user={single(root)}>
        <ApiKeysPage />
      </As>,
    );
    expect(await screen.findByText("No API keys yet")).toBeInTheDocument();
    // The subtitle says what keys are for; the empty state does not repeat it.
    expect(screen.getByText(/^For scripts and other tools/)).toBeInTheDocument();
    expect(screen.queryByText(/use MonitorFlow as you would/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "New API key" }));
    await userEvent.type(screen.getByLabelText("What is it for?"), "Ticketing");
    await userEvent.selectOptions(screen.getByLabelText(/^Access/), "operator");
    await userEvent.click(screen.getByLabelText("Only some areas"));
    // One customer (ADR-0104): no customer in the reaches or the words.
    expect(screen.getByText("Any key may list servers and sites within its reach.")).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "paris" })).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Servers"));
    await userEvent.click(screen.getByRole("button", { name: "Make the key" }));

    expect(await screen.findByDisplayValue("mfk_secret")).toBeInTheDocument();
    expect(screen.getByText(/it is shown only once/)).toBeInTheDocument();
    const post = fetchMock.mock.calls.find((c) => c[1]?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({
      name: "Ticketing",
      role: "operator",
      expires: "1y",
      areas: ["alerts"],
    });
    expect(await screen.findByText("mfk_abcd1234…")).toBeInTheDocument();
    expect(screen.getByText(/Read and change · Alerts and checks · Everything\. Made by alice\./)).toBeInTheDocument();
    expect(screen.queryByText(/acme|customer/)).not.toBeInTheDocument();
  });

  it("offers customers to someone who sees several", async () => {
    stubFetch({
      "GET /api/api-keys": { status: 200, body: { keys: [] } },
      "GET /api/customers": { status: 200, body: { customers: [ACME, GLOBEX] } },
      "GET /api/sites": { status: 200, body: { sites: [PARIS, BERLIN] } },
    });
    render(
      <As user={{ ...root, multi_customer: true }}>
        <ApiKeysPage />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "New API key" }));
    expect(await screen.findByRole("option", { name: "globex" })).toBeInTheDocument();
    expect(screen.getByRole("option", { name: "acme › paris" })).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Only some areas"));
    expect(screen.getByText("Any key may list servers, sites and customers within its reach.")).toBeInTheDocument();
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
    await userEvent.click(await screen.findByRole("button", { name: "Turn off Ticketing" }));
    await userEvent.click(screen.getByRole("button", { name: "Turn off" }));
    expect(fetchMock.mock.calls.some((c) => String(c[0]) === "/api/api-keys/k1" && c[1]?.method === "DELETE")).toBe(
      true,
    );
    expect(await screen.findByText("No API keys yet")).toBeInTheDocument();
  });
});
