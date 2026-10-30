import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { UserAccount } from "@/lib/api";
import { As, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { UsersAdminPage, describeGrant } from "./UsersAdmin";

const names = {
  customers: [{ id: "c1", name: "Acme Hosting", site_count: 2 }],
  sites: [
    { id: "s2", name: "lyon-office", server_count: 0, relay_count: 0, customer_id: "c1", customer_name: "Acme", agent_auto_update: null, agent_auto_update_effective: true },
  ],
};

const root = me({ role: "administrator", customer_id: null, site_id: null });

const account = (overrides: Partial<UserAccount>): UserAccount => ({
  id: "u1",
  username: "alice",
  state: "active",
  created_at: "2026-10-28T10:00:00Z",
  roles: [{ role: "administrator", customer_id: null, site_id: null }],
  link_expires_at: null,
  ...overrides,
});

const bob = account({
  id: "u2",
  username: "bob",
  state: "invited",
  roles: [{ role: "viewer", customer_id: null, site_id: "s2" }],
  link_expires_at: "2026-11-04T10:00:00Z",
});

const NAMES = {
  "GET /api/customers": { status: 200, body: { customers: names.customers } },
  "GET /api/sites": { status: 200, body: { sites: names.sites } },
};

function renderAsRoot() {
  render(
    <As user={root}>
      <UsersAdminPage />
    </As>,
  );
}

const sentBody = (fetchMock: ReturnType<typeof stubFetch>, method: string, url: string) =>
  JSON.parse(fetchMock.mock.calls.find(([u, init]) => u === url && init?.method === method)?.[1]?.body as string);

describe("UsersAdminPage", () => {
  it("invites a user and shows the link to copy", async () => {
    const fetchMock = stubFetch({
      ...NAMES,
      "GET /api/users": { status: 200, body: { users: [account({})] } },
      "POST /api/users": { status: 201, body: { user: bob, link: { token: "ab".repeat(32), expires_at: bob.link_expires_at } } },
    });
    const user = userEvent.setup();
    renderAsRoot();
    expect(await screen.findByText("Only you so far")).toBeInTheDocument();
    const form = screen.getByRole("form", { name: "Invite a user" });
    await user.type(within(form).getByLabelText("Username"), "bob");
    await user.selectOptions(within(form).getByLabelText("Role"), "operator");
    await user.selectOptions(within(form).getByLabelText("On"), "Site lyon-office");
    await user.click(within(form).getByRole("button", { name: "Create invitation link" }));

    const link = await screen.findByLabelText("Invitation link for bob");
    expect(link).toHaveValue(`${window.location.origin}/welcome/#${"ab".repeat(32)}`);
    expect(screen.getByRole("button", { name: "Copy link" })).toBeInTheDocument();
    expect(sentBody(fetchMock, "POST", "/api/users")).toEqual({ username: "bob", role: "operator", site_id: "s2" });
  });

  it("lists users with their roles, and acts on them", async () => {
    const fetchMock = stubFetch({
      ...NAMES,
      "GET /api/users": { status: 200, body: { users: [account({}), bob] } },
      "DELETE /api/users/u2/roles": { status: 200, body: { ...bob, roles: [] } },
      "PUT /api/users/u2/disabled": { status: 200, body: { ...bob, state: "disabled" } },
      "POST /api/users/u2/link": { status: 201, body: { token: "cd".repeat(32), expires_at: bob.link_expires_at } },
    });
    const user = userEvent.setup();
    renderAsRoot();
    const table = await screen.findByRole("table", { name: "Users" });
    const [, mine, theirs] = within(table).getAllByRole("row");
    expect(mine).toHaveTextContent(/alice.*You.*Active.*administrator · everything/);
    // No way to lock yourself out from your own row.
    expect(within(mine!).queryByRole("button", { name: "Disable" })).not.toBeInTheDocument();
    expect(within(mine!).queryByRole("button", { name: /Remove/ })).not.toBeInTheDocument();
    expect(theirs).toHaveTextContent(/bob.*Has not opened the invitation yet.*Invited/);
    expect(await within(theirs!).findByText("viewer · Acme › lyon-office")).toBeInTheDocument();
    expect(screen.queryByText("Only you so far")).not.toBeInTheDocument();

    await user.click(within(theirs!).getByRole("button", { name: "Revoke viewer · Acme › lyon-office from bob" }));
    await vi.waitFor(() =>
      expect(sentBody(fetchMock, "DELETE", "/api/users/u2/roles")).toEqual({ role: "viewer", site_id: "s2" }),
    );
    await user.click(within(theirs!).getByRole("button", { name: "Disable" }));
    await vi.waitFor(() => expect(sentBody(fetchMock, "PUT", "/api/users/u2/disabled")).toEqual({ disabled: true }));
    await user.click(within(theirs!).getByRole("button", { name: "New invitation link" }));
    expect(await screen.findByLabelText("Invitation link for bob")).toHaveValue(
      `${window.location.origin}/welcome/#${"cd".repeat(32)}`,
    );
  });

  it("explains why a change was refused", async () => {
    stubFetch({
      ...NAMES,
      "GET /api/users": { status: 200, body: { users: [account({}), account({ id: "u3", username: "carol" })] } },
      "DELETE /api/users/u3/roles": {
        status: 409,
        body: { error: "this would leave no active administrator of everything; make another user one first" },
      },
    });
    const user = userEvent.setup();
    renderAsRoot();
    const table = await screen.findByRole("table", { name: "Users" });
    await user.click(within(table).getByRole("button", { name: "Revoke administrator · everything from carol" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/^This would leave no active administrator/);
  });

  it("shows other administrators their own account only", async () => {
    stubFetch(NAMES);
    const bobAdmin = me(
      { role: "administrator", customer_id: "c1", site_id: null },
      { role: "viewer", customer_id: null, site_id: "s2" },
    );
    render(
      <As user={bobAdmin}>
        <UsersAdminPage />
      </As>,
    );
    const table = screen.getByRole("table", { name: "Users" });
    const [, row] = within(table).getAllByRole("row");
    expect(row).toHaveTextContent(/alice.*Active/);
    expect(await within(row!).findByText(/administrator · Acme Hosting\s+·\s+viewer · Acme › lyon-office/)).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
    expect(screen.getByText(/Only an administrator of everything invites users/)).toBeInTheDocument();
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
    expect(describeGrant({ role: "viewer", customer_id: null, site_id: null }, undefined)).toBe("viewer · everything");
    expect(describeGrant({ role: "operator", customer_id: "c9", site_id: null }, names)).toBe("operator · a customer");
  });
});
