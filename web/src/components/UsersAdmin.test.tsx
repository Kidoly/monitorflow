import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { UsersAdminPage, describeGrant } from "./UsersAdmin";

const names = {
  customers: [{ id: "c1", name: "Acme Hosting", site_count: 2 }],
  sites: [
    { id: "s2", name: "lyon-office", server_count: 0, relay_count: 0, customer_id: "c1", customer_name: "Acme" },
  ],
};

describe("UsersAdminPage", () => {
  it("lists the signed-in account and its grants, without actions the API lacks", async () => {
    stubFetch({
      "GET /api/customers": { status: 200, body: { customers: names.customers } },
      "GET /api/sites": { status: 200, body: { sites: names.sites } },
    });
    const bob = me(
      { role: "administrator", customer_id: "c1", site_id: null },
      { role: "viewer", customer_id: null, site_id: "s2" },
    );
    render(
      <As user={bob}>
        <UsersAdminPage />
      </As>,
    );
    const table = screen.getByRole("table", { name: "Users" });
    const [, row] = within(table).getAllByRole("row");
    expect(row).toHaveTextContent(/alice.*Active/);
    expect(await within(row!).findByText(/administrator · Acme Hosting\s+·\s+viewer · Acme › lyon-office/)).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByText(/monitorflow-server user grant/)).toBeInTheDocument();
  });

  it("is only shown to administrators", () => {
    const fetchMock = stubFetch({});
    render(
      <As user={viewer}>
        <UsersAdminPage />
      </As>,
    );
    expect(screen.getByText(/needs an administrator role/)).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("describes grants before the names arrive", () => {
    expect(describeGrant({ role: "viewer", customer_id: null, site_id: null }, undefined)).toBe("viewer · global");
    expect(describeGrant({ role: "operator", customer_id: "c9", site_id: null }, names)).toBe("operator · a customer");
  });
});
