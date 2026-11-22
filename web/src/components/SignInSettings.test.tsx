import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { IdentityProvider, IdentityProviderList } from "@/lib/api";
import { As, customerAdmin, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { SignInSettingsPage } from "./SignInSettings";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = single(me({ role: "administrator", customer_id: null, site_id: null }));
const LIST = "GET /api/identity-providers";

const keycloak: IdentityProvider = {
  id: "p1",
  kind: "oidc",
  name: "Keycloak",
  enabled: false,
  has_secret: true,
  settings: {
    kind: "oidc",
    issuer: "https://id.example.com/realms/acme",
    client_id: "monitorflow",
    scopes: "openid profile email groups",
    groups_claim: "groups",
    ca_pem: null,
  },
  mappings: [{ group: "mf-admins", role: "administrator", customer_id: null, site_id: null }],
  accounts: 2,
  created_at: "2026-11-17T08:00:00Z",
  updated_at: "2026-11-17T08:00:00Z",
};

function list(providers: IdentityProvider[]): { status: number; body: IdentityProviderList } {
  return {
    status: 200,
    body: { providers, redirect_uri: "https://mf.example/api/auth/oidc/callback", local_administrators: 1 },
  };
}

const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <SignInSettingsPage />
    </As>,
  );

const sent = (fetchMock: ReturnType<typeof stubFetch>, key: string) => {
  const call = fetchMock.mock.calls.find(([url, init]) => `${init?.method ?? "GET"} ${String(url)}` === key);
  return JSON.parse(String(call?.[1]?.body));
};

describe("SignInSettingsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("is for global administrators only", () => {
    page(customerAdmin);
    expect(screen.getByText("Sign-in settings need an administrator of everything.")).toBeInTheDocument();
  });

  it("says how people sign in today and that a local administrator stays", async () => {
    stubFetch({ [LIST]: list([]) });
    page();
    expect(await screen.findByText(/People sign in with a password kept by MonitorFlow/)).toBeInTheDocument();
    expect(screen.getByText(/1 local administrator keeps a password/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add OpenID Connect provider" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add LDAP directory" })).toBeInTheDocument();
  });

  it("turns a provider on only once its test passed", async () => {
    const fetchMock = stubFetch({
      [LIST]: list([]),
      "POST /api/identity-providers/test": {
        status: 200,
        body: { ok: true, error: null, oidc: { issuer: "https://id.example.com/realms/acme", keys: 2, client_checked: true }, ldap: null, roles: [] },
      },
      "POST /api/identity-providers": { status: 201, body: { ...keycloak, enabled: true } },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add OpenID Connect provider" }));
    const form = screen.getByRole("form", { name: "New OpenID Connect provider" });
    await user.type(within(form).getByLabelText(/^Name/), "Keycloak");
    await user.type(within(form).getByLabelText(/^Issuer address/), "https://id.example.com/realms/acme");
    await user.type(within(form).getByLabelText(/^Client ID/), "monitorflow");
    await user.type(within(form).getByLabelText(/^Client secret/), "s3cret");
    expect(within(form).getByText("https://mf.example/api/auth/oidc/callback")).toBeInTheDocument();
    const turnOn = within(form).getByRole("button", { name: "Save and turn on" });
    expect(turnOn).toBeDisabled();
    await user.click(within(form).getByRole("button", { name: "Test connection" }));
    expect(await within(form).findByText("https://id.example.com/realms/acme answered with 2 signing keys.")).toBeInTheDocument();
    expect(turnOn).toBeEnabled();
    await user.click(turnOn);
    await vi.waitFor(() => expect(sent(fetchMock, "POST /api/identity-providers")).toMatchObject({
      name: "Keycloak",
      enabled: true,
      secret: "s3cret",
      settings: { kind: "oidc", issuer: "https://id.example.com/realms/acme", client_id: "monitorflow" },
    }));
  });

  it("shows why a test failed", async () => {
    stubFetch({
      [LIST]: list([keycloak]),
      "POST /api/identity-providers/test": {
        status: 200,
        body: { ok: false, error: "The provider refused the client ID or the client secret.", oidc: null, ldap: null, roles: [] },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("button", { name: "Test connection" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("The provider refused the client ID or the client secret.");
    expect(screen.getByRole("button", { name: "Save and turn on" })).toBeDisabled();
  });

  it("maps groups to roles without places for a single company", async () => {
    const fetchMock = stubFetch({
      [LIST]: list([keycloak]),
      "PUT /api/identity-providers/p1/groups": {
        status: 200,
        body: { mappings: [...keycloak.mappings, { group: "helpdesk", role: "viewer", customer_id: null, site_id: null }] },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const groups = screen.getByRole("form", { name: "Groups and roles" });
    expect(within(groups).queryByLabelText("Where")).not.toBeInTheDocument();
    await user.click(within(groups).getByRole("button", { name: "Add a group" }));
    const fields = within(groups).getAllByLabelText("Group");
    await user.type(fields[1]!, "helpdesk");
    await user.click(within(groups).getByRole("button", { name: "Save groups" }));
    await vi.waitFor(() =>
      expect(sent(fetchMock, "PUT /api/identity-providers/p1/groups")).toEqual({
        mappings: [
          { group: "mf-admins", role: "administrator", customer_id: null, site_id: null },
          { group: "helpdesk", role: "viewer", customer_id: null, site_id: null },
        ],
      }),
    );
  });

  it("sets the encryption from a directory's address and never offers plain LDAP", async () => {
    stubFetch({ [LIST]: list([]) });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add LDAP directory" }));
    const form = screen.getByRole("form", { name: "New LDAP directory" });
    await user.type(within(form).getByLabelText(/^Directory address/), "ldap://ldap.example.com");
    expect(within(form).getByRole("radio", { name: "StartTLS (port 389)" })).toBeChecked();
    await user.click(within(form).getByRole("radio", { name: "LDAPS (port 636)" }));
    expect(within(form).getByLabelText(/^Directory address/)).toHaveValue("ldaps://ldap.example.com");
    expect(within(form).getAllByRole("radio", { name: /port/ })).toHaveLength(2);
  });
});
