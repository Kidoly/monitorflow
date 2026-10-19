import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { CSRF_HEADER } from "@/lib/api";
import { customerAdmin } from "@/test/admin";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { SitesAdmin } from "./SitesAdmin";

const site = (id: string, name: string, customer_id: string, customer_name: string) => ({
  id,
  name,
  server_count: 3,
  relay_count: 1,
  customer_id,
  customer_name,
});

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
      "POST /api/customers/c1/sites": { status: 201, body: site("s3", "nice", "c1", "acme") },
    });
    render(<SitesAdmin me={customerAdmin} />);
    const acme = await screen.findByRole("region", { name: "acme" });
    expect(within(acme).getByRole("row", { name: /paris/ })).toHaveTextContent(/paris\s*3\s*1/);
    const globex = screen.getByRole("region", { name: "globex" });
    expect(within(globex).queryByRole("form")).not.toBeInTheDocument();

    const form = within(acme).getByRole("form", { name: "New site for acme" });
    await userEvent.type(within(form).getByLabelText("New site name"), " nice ");
    await userEvent.click(within(form).getByRole("button", { name: "Create site" }));
    expect(await within(form).findByRole("status")).toHaveTextContent("Created site nice.");
    const call = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "POST");
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
    expect(fetchMock.mock.calls[call]?.[1]?.body).toBe(JSON.stringify({ name: "nice" }));
    expect(await within(acme).findByRole("row", { name: /nice/ })).toBeInTheDocument();
  });

  it("shows why a creation failed", async () => {
    stubFetch({
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 0 }] } },
      "GET /api/sites": { status: 200, body: { sites: [] } },
      "POST /api/customers/c1/sites": { status: 409, body: { error: "a site with this name already exists" } },
    });
    render(<SitesAdmin me={customerAdmin} />);
    await userEvent.type(await screen.findByLabelText("New site name"), "paris");
    await userEvent.click(screen.getByRole("button", { name: "Create site" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("already exists");
  });
});
