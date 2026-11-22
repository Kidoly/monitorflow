import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { connection } from "@/test/psa";

import { HaloPsaPage, sameAddress } from "./HaloPsa";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const CONNECTION = "/api/psa/connection";
const none = { status: 200, body: { connection: null } };

const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <HaloPsaPage />
    </As>,
  );

function sent(fetchMock: ReturnType<typeof stubFetch>, method: string, url: string) {
  const call = fetchMock.mock.calls.find(([u, init]) => u === url && init?.method === method);
  return JSON.parse(String(call?.[1]?.body));
}

describe("sameAddress", () => {
  it("ignores slashes and /api as the server does", () => {
    expect(sameAddress("https://halo.example.com/api/", "https://halo.example.com")).toBe(true);
    expect(sameAddress("https://other.example.com", "https://halo.example.com")).toBe(false);
  });
});

describe("HaloPsaPage", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    setActiveLocale("en");
  });

  it("is for global administrators", () => {
    page(customerAdmin);
    expect(screen.getByText("HaloPSA needs a global administrator")).toBeInTheDocument();
  });

  it("says where to create the application, tests, then saves", async () => {
    const fetchMock = stubFetch({
      [`GET ${CONNECTION}`]: [none, { status: 200, body: { connection: connection() } }],
      [`POST ${CONNECTION}/test`]: { status: 200, body: { ok: true, agent_name: "MonitorFlow", error: null } },
      [`PUT ${CONNECTION}`]: { status: 200, body: connection() },
      "GET /api/psa/site-links": { status: 200, body: { sites: [] } },
    });
    const user = userEvent.setup();
    page();
    expect(await screen.findByText(/Client ID and Secret \(Client Credentials\)/)).toBeInTheDocument();
    expect(screen.getByText("Under Permissions, tick all.")).toBeInTheDocument();
    const testButton = screen.getByRole("button", { name: "Test connection" });
    expect(testButton).toBeDisabled();
    await user.type(screen.getByLabelText(/^Address/), "https://halo.example.com");
    await user.type(screen.getByLabelText(/^Client ID/), "id-1");
    await user.type(screen.getByLabelText(/^Client Secret/), "s3cret");
    await user.click(testButton);
    expect(await screen.findByText("Connected as MonitorFlow")).toBeInTheDocument();
    expect(sent(fetchMock, "POST", `${CONNECTION}/test`)).toEqual({
      base_url: "https://halo.example.com",
      tenant: null,
      client_id: "id-1",
      client_secret: "s3cret",
      enabled: true,
    });
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(sent(fetchMock, "PUT", CONNECTION).client_secret).toBe("s3cret");
    expect(await screen.findByRole("button", { name: "Remove the connection" })).toBeInTheDocument();
  });

  it("keeps the saved secret for the same address only, and shows Halo's error", async () => {
    stubFetch({
      [`GET ${CONNECTION}`]: { status: 200, body: { connection: connection() } },
      "GET /api/psa/site-links": { status: 200, body: { sites: [] } },
      [`POST ${CONNECTION}/test`]: {
        status: 200,
        body: { ok: false, agent_name: null, error: "halo.example.com answered 401 Unauthorized to the sign-in: invalid_client" },
      },
    });
    const user = userEvent.setup();
    page();
    const secret = await screen.findByLabelText(/^Client Secret/);
    expect(secret).toHaveAttribute("placeholder", "Saved; leave empty to keep it");
    expect(secret).toHaveValue("");
    await user.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("invalid_client");
    const address = screen.getByLabelText(/^Address/);
    await user.clear(address);
    await user.type(address, "https://other.example.com");
    expect(screen.getByText("Enter it again: the address changed.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Test connection" })).toBeDisabled();
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    stubFetch({ [`GET ${CONNECTION}`]: none });
    page();
    expect(await screen.findByRole("button", { name: "Tester la connexion" })).toBeInTheDocument();
    expect(screen.getByText("Où les trouver dans HaloPSA")).toBeInTheDocument();
  });
});
