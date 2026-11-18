import { render, screen, within } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { WebsiteCheck } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { WebsiteChecks } from "./WebsiteChecks";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ROUTE = "GET /api/servers/srv-1/website-checks";
const DAY = 86_400_000;

const base: WebsiteCheck = {
  name: "http_shop_example_com",
  instance: "",
  entry_id: "e1",
  origin: "server",
  url: "https://shop.example.com/",
  run_by: "agent",
  relay: null,
  interval_ms: 60_000,
  last_at: new Date().toISOString(),
  up: true,
  reason: null,
  status_code: 200,
  response_seconds: 0.212,
  redirects: 1,
  final_url: "https://www.shop.example.com/",
  content_ok: true,
  certificate: {
    ok: true,
    reason: null,
    name_ok: true,
    days_left: 41.6,
    not_after: new Date(Date.now() + 41.6 * DAY).toISOString(),
    issuer: "Test CA",
    subject: "www.shop.example.com",
  },
};

const failing: WebsiteCheck = {
  ...base,
  name: "http_api_example_com",
  url: "https://api.example.com/health",
  run_by: "relay",
  relay: "relay-paris",
  up: false,
  reason: "status",
  status_code: 503,
  response_seconds: 1.5,
  redirects: 0,
  final_url: "https://api.example.com/health",
  certificate: { ...base.certificate!, ok: false, reason: "expired", days_left: -2.3 },
};

const fresh: WebsiteCheck = {
  ...base,
  name: "http_intranet",
  url: "http://intranet/",
  run_by: "central",
  last_at: null,
  up: null,
  status_code: null,
  response_seconds: null,
  redirects: null,
  final_url: null,
  certificate: null,
};

afterEach(() => setActiveLocale("en"));

describe("WebsiteChecks", () => {
  it("shows each website's result, time, certificate and who checks it", async () => {
    stubFetch({ [ROUTE]: { status: 200, body: { checks: [base, failing, fresh] } } });
    render(<WebsiteChecks serverId="srv-1" />);
    const table = await screen.findByRole("table", { name: "Websites" });
    const rows = within(table).getAllByRole("row").slice(1);
    expect(rows[0]).toHaveTextContent("shop.example.com");
    expect(rows[0]).toHaveTextContent("→ https://www.shop.example.com/");
    expect(rows[0]).toHaveTextContent("Answers");
    expect(rows[0]).toHaveTextContent("212 ms");
    expect(rows[0]).toHaveTextContent("41 days left");
    expect(rows[0]).toHaveTextContent("from Test CA");
    expect(rows[0]).toHaveTextContent("This server's agent");
    expect(rows[1]).toHaveTextContent("Answered 503, which is not expected");
    expect(rows[1]).toHaveTextContent("Fails");
    expect(rows[1]).toHaveTextContent("Expired");
    expect(rows[1]).toHaveTextContent("Relay relay-paris");
    expect(rows[2]).toHaveTextContent("Waiting for a first run");
    expect(rows[2]).toHaveTextContent("None");
    expect(rows[2]).toHaveTextContent("Central server");
  });

  it("shows nothing without website checks, and speaks French", async () => {
    stubFetch({ [ROUTE]: { status: 200, body: { checks: [] } } });
    const { container } = render(<WebsiteChecks serverId="srv-1" />);
    await new Promise((r) => setTimeout(r, 10));
    expect(container).toBeEmptyDOMElement();

    setActiveLocale("fr");
    stubFetch({ [ROUTE]: { status: 200, body: { checks: [failing] } } });
    render(<WebsiteChecks serverId="srv-1" />);
    const table = await screen.findByRole("table", { name: "Sites web" });
    expect(table).toHaveTextContent("A répondu 503, qui n’est pas attendu");
    expect(table).toHaveTextContent("Expiré");
    expect(table).toHaveTextContent("Relais relay-paris");
  });
});
