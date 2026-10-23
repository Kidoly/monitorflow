import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { CSRF_HEADER } from "@/lib/api";
import { customerAdmin, relay } from "@/test/admin";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { SitesAdmin } from "./SitesAdmin";

const C1 = "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1c01";

const site = (id: string, name: string, customer_id: string, customer_name: string) => ({
  id,
  name,
  server_count: 3,
  relay_count: 1,
  customer_id,
  customer_name,
});

const servers = {
  status: 200,
  body: {
    servers: [
      { id: "a", site_id: "s1", status: "online", revoked_at: null },
      { id: "b", site_id: "s1", status: "offline", revoked_at: null },
    ],
    next_cursor: null,
  },
};
const relays = {
  status: 200,
  body: { relays: [relay({ site_id: "s1", peer_id: "relay-paris-1" })], next_cursor: null },
};

describe("SitesAdmin", () => {
  it("groups sites by customer and creates sites only where allowed", async () => {
    const paris = site("s1", "paris", "c1", "acme");
    const fetchMock = stubFetch({
      "GET /api/customers": {
        status: 200,
        body: {
          customers: [
            { id: "c1", name: "acme", site_count: 1 },
            { id: "c2", name: "globex", site_count: 1 },
          ],
        },
      },
      "GET /api/sites": [
        { status: 200, body: { sites: [paris, site("s2", "lyon", "c2", "globex")] } },
        { status: 200, body: { sites: [paris, site("s3", "nice", "c1", "acme")] } },
      ],
      "GET /api/servers": servers,
      "GET /api/relays": relays,
      "POST /api/customers/c1/sites": { status: 201, body: site("s3", "nice", "c1", "acme") },
    });
    render(<SitesAdmin me={customerAdmin} />);
    const acme = await screen.findByRole("region", { name: "acme" });
    expect(acme).toHaveTextContent("1 site · 3 servers · 1 relay");
    expect(within(acme).getByRole("row", { name: /paris/ })).toHaveTextContent(/paris\s*3 · 1 online\s*1 · relay-paris-1/);
    const globex = screen.getByRole("region", { name: "globex" });
    expect(within(globex).queryByRole("form")).not.toBeInTheDocument();
    expect(screen.getByText(/2 customers, 2 sites/)).toBeInTheDocument();
    expect(screen.getByText(/cannot be renamed or moved to another customer/)).toBeInTheDocument();

    const form = within(acme).getByRole("form", { name: "New site for acme" });
    await userEvent.type(within(form).getByLabelText("New site name"), " nice ");
    await userEvent.click(within(form).getByRole("button", { name: "Create site" }));
    expect(await within(form).findByRole("status")).toHaveTextContent("Created site nice.");
    const call = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "POST");
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
    expect(fetchMock.mock.calls[call]?.[1]?.body).toBe(JSON.stringify({ name: "nice" }));
    expect(await within(acme).findByRole("row", { name: /nice/ })).toBeInTheDocument();
  });

  it("links each site to the fleet scoped on it", async () => {
    stubFetch({
      "GET /api/customers": { status: 200, body: { customers: [{ id: C1, name: "acme", site_count: 1 }] } },
      "GET /api/sites": { status: 200, body: { sites: [site("6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1c02", "paris", C1, "acme")] } },
      "GET /api/servers": servers,
      "GET /api/relays": relays,
    });
    render(<SitesAdmin me={customerAdmin} />);
    expect(await screen.findByRole("link", { name: "paris" })).toHaveAttribute(
      "href",
      `/?customer=${C1}&site=6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1c02`,
    );
  });

  it("refuses names outside the permanent-name rule without asking the API", async () => {
    const fetchMock = stubFetch({
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 0 }] } },
      "GET /api/sites": { status: 200, body: { sites: [] } },
      "GET /api/servers": servers,
      "GET /api/relays": relays,
    });
    render(<SitesAdmin me={customerAdmin} />);
    const input = await screen.findByLabelText("New site name");
    expect(screen.getByText("Lowercase letters, digits and dashes. Permanent.")).toBeInTheDocument();
    await userEvent.type(input, "Paris DC");
    await userEvent.click(screen.getByRole("button", { name: "Create site" }));
    expect(screen.getByRole("alert")).toHaveTextContent("lowercase letters, digits and dashes only");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);
  });

  it("shows why a creation failed", async () => {
    stubFetch({
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 0 }] } },
      "GET /api/sites": { status: 200, body: { sites: [] } },
      "GET /api/servers": servers,
      "GET /api/relays": relays,
      "POST /api/customers/c1/sites": { status: 409, body: { error: "a site with this name already exists" } },
    });
    render(<SitesAdmin me={customerAdmin} />);
    await userEvent.type(await screen.findByLabelText("New site name"), "paris");
    await userEvent.click(screen.getByRole("button", { name: "Create site" }));
    expect(await screen.findByRole("alert")).toHaveTextContent('A site named "paris" already exists.');
  });
});
