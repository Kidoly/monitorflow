import { render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { As, customerAdmin, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { connection } from "@/test/psa";

import { IntegrationsCatalogue, psaStatus } from "./IntegrationsCatalogue";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const noWebhooks = { status: 200, body: { endpoints: [], events: [] } };


const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <IntegrationsCatalogue />
    </As>,
  );

describe("psaStatus", () => {
  it("says off, failing, connected or not tested", () => {
    expect(psaStatus(null)).toBeUndefined();
    expect(psaStatus(connection({ enabled: false }))?.text).toBe("Off");
    expect(psaStatus(connection({ last_error: "no", last_error_at: "2026-11-22T10:00:00Z" }))?.tone).toBe("bad");
    expect(psaStatus(connection({ last_error: "old", last_error_at: "2026-11-22T08:30:00Z" }))?.text).toBe(
      "Connected as MonitorFlow",
    );
    expect(psaStatus(connection({ secret_error: "key missing" }))?.tone).toBe("bad");
    expect(psaStatus(connection({ agent_name: null, last_success_at: null }))?.text).toBe("Saved, not tested yet");
  });
});

describe("IntegrationsCatalogue", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setActiveLocale("en");
  });

  it("is for administrators", () => {
    page(viewer);
    expect(screen.getByText("Integrations need an administrator")).toBeInTheDocument();
  });

  it("offers HaloPSA and custom webhooks when nothing is set up", async () => {
    stubFetch({ "GET /api/integrations": noWebhooks, "GET /api/psa/connection": { status: 200, body: { connection: null } } });
    page();
    const available = await screen.findByRole("region", { name: "Available" });
    const halo = within(available).getByRole("link", { name: /HaloPSA/ });
    expect(halo).toHaveAttribute("href", "/settings/integrations/halopsa");
    expect(halo).toHaveTextContent("Set up");
    expect(within(available).getByRole("link", { name: /Custom webhook/ })).toHaveAttribute(
      "href",
      "/settings/integrations/webhooks",
    );
    expect(screen.queryByRole("region", { name: "Set up" })).not.toBeInTheDocument();
  });

  it("puts what is set up first, with its state", async () => {
    stubFetch({
      "GET /api/integrations": noWebhooks,
      "GET /api/psa/connection": { status: 200, body: { connection: connection() } },
    });
    page();
    const configured = await screen.findByRole("region", { name: "Set up" });
    expect(within(configured).getByRole("link", { name: /HaloPSA/ })).toHaveTextContent("Connected as MonitorFlow");
  });

  it("tells a customer's administrator who sets up HaloPSA", async () => {
    const fetchMock = stubFetch({ "GET /api/integrations": noWebhooks });
    page(customerAdmin);
    expect(await screen.findByText("A global administrator sets it up.")).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /HaloPSA/ })).not.toBeInTheDocument();
    expect(fetchMock.mock.calls.map(([url]) => url)).not.toContain("/api/psa/connection");
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ "GET /api/integrations": noWebhooks, "GET /api/psa/connection": { status: 200, body: { connection: null } } });
    page();
    expect(await screen.findByRole("link", { name: /Webhook personnalisé/ })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Disponibles" })).toBeInTheDocument();
  });
});
