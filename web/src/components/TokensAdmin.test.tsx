import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { CSRF_HEADER, type CreatedToken, type EnrollmentToken } from "@/lib/api";
import { customerAdmin, siteAdmin } from "@/test/admin";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { TokensAdmin, configSnippet } from "./TokensAdmin";

const SECRET = "mfe1.c2VjcmV0.Y2E";

const token = (overrides: Partial<EnrollmentToken> = {}): EnrollmentToken => ({
  id: "t1",
  role: "agent",
  peer_id: "web-01",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  server_names: [],
  created_by: "u1",
  created_by_username: "alice",
  created_at: "2026-10-22T10:00:00Z",
  expires_at: "2026-10-22T11:00:00Z",
  used_at: null,
  state: "pending",
  ...overrides,
});

const created: CreatedToken = {
  ...token({ id: "t2", role: "relay", peer_id: "relay-1", server_names: ["relay1.example"] }),
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  agent_download_url: "https://github.com/Kidoly/monitorflow/releases/latest/download",
};

const sites = {
  status: 200,
  body: {
    sites: [
      { id: "site-1", name: "paris", server_count: 0, relay_count: 0, customer_id: "c1", customer_name: "acme" },
      { id: "site-9", name: "lyon", server_count: 0, relay_count: 0, customer_id: "c9", customer_name: "other" },
    ],
  },
};

describe("TokensAdmin", () => {
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("creates a relay token and shows it once with its configuration", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/tokens": [
        { status: 200, body: { tokens: [], next_cursor: null } },
        { status: 200, body: { tokens: [token({ id: "t2", peer_id: "relay-1", role: "relay" })], next_cursor: null } },
      ],
      "GET /api/sites": sites,
      "POST /api/tokens": { status: 201, body: created },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New token" });
    expect(form).toHaveTextContent("Recorded in the audit log as alice.");
    // Only sites of administered customers are offered.
    const site = within(form).getByLabelText("Site");
    await within(site).findByRole("option");
    expect(within(site).getAllByRole("option").map((o) => o.textContent)).toEqual(["acme › paris"]);
    // Server names are for relays only.
    expect(within(form).getByLabelText(/Server names/)).toBeDisabled();
    await user.click(within(form).getByRole("radio", { name: "Relay" }));
    await user.type(within(form).getByLabelText("Peer ID"), "relay-1");
    await user.selectOptions(within(form).getByLabelText("Expires in"), "86400");
    await user.type(within(form).getByLabelText(/Server names/), "relay1.example, 10.0.0.5");
    await user.click(within(form).getByRole("button", { name: "Create token" }));

    const panel = await screen.findByRole("region", { name: /^Token for relay-1 · expires/ });
    expect(within(panel).getByRole("textbox", { name: "Token" })).toHaveValue(SECRET);
    // The warning comes before the secret.
    const alert = within(panel).getByRole("alert");
    expect(alert).toHaveTextContent("This token will not be shown again.");
    expect(alert.compareDocumentPosition(within(panel).getByRole("textbox"))).toBe(Node.DOCUMENT_POSITION_FOLLOWING);
    expect(within(panel).getByLabelText(/Configuration snippet/)).toHaveTextContent(
      `upstream_url = "https://mf.example:7443" enroll_url = "https://mf.example:7445" enrollment_token = "${SECRET}"`,
    );
    expect(panel).toHaveTextContent("/etc/monitorflow/relay.toml");
    const post = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "POST");
    expect(requestHeaders(fetchMock, post)[CSRF_HEADER]).toBe("1");
    expect(JSON.parse(String(fetchMock.mock.calls[post]?.[1]?.body))).toEqual({
      role: "relay",
      peer_id: "relay-1",
      site_id: "site-1",
      ttl_s: 86400,
      server_names: ["relay1.example", "10.0.0.5"],
    });

    await user.click(within(panel).getByRole("button", { name: "Copy token" }));
    expect(await navigator.clipboard.readText()).toBe(SECRET);
    expect(within(panel).getByRole("button", { name: "Copy token" })).toHaveTextContent("Copied");
    await user.click(within(panel).getByRole("button", { name: "Copy configuration snippet" }));
    expect(await navigator.clipboard.readText()).toBe(configSnippet(created));

    // Once dismissed, nothing on the page shows the token again.
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.queryByDisplayValue(SECRET)).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain(SECRET);
    expect(await screen.findByRole("row", { name: /relay-1/ })).toBeInTheDocument();
  });

  it("shows the install commands for an agent token, through a relay if asked", async () => {
    const user = userEvent.setup();
    const agentToken: CreatedToken = { ...created, ...token({ id: "t4", peer_id: "web-03" }) };
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "POST /api/tokens": { status: 201, body: agentToken },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New token" });
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Peer ID"), "web-03");
    await user.click(within(form).getByRole("button", { name: "Create token" }));

    const install = await screen.findByRole("group", { name: "Install the agent" });
    const linux = within(install).getByLabelText(/^Linux/);
    expect(linux).toHaveTextContent(`| sudo MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`);
    expect(within(install).getByLabelText(/^Windows/)).toHaveTextContent(
      `UPSTREAM_URL=https://mf.example:7443 ENROLLMENT_TOKEN=${SECRET}`,
    );
    await user.click(within(install).getByRole("button", { name: "Copy the Linux command" }));
    expect(await navigator.clipboard.readText()).toBe(linux.textContent);

    const relay = within(install).getByLabelText(/Connect through a relay/);
    await user.type(relay, "relay1.paris.example:7443");
    expect(linux).toHaveTextContent("MF_SERVER=relay1.paris.example:7443 MF_ENROLL=mf.example:7445");
    await user.type(relay, "; id");
    expect(relay).toHaveAttribute("aria-invalid", "true");
    expect(within(install).getByRole("alert")).toHaveTextContent("Enter a host name or IP address");
    expect(linux).toHaveTextContent("MF_SERVER=mf.example:7443 MF_TOKEN");
    expect(linux).not.toHaveTextContent("; id");
  });

  it("shows no install commands for a relay token", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "POST /api/tokens": { status: 201, body: created },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New token" });
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.click(within(form).getByRole("radio", { name: "Relay" }));
    await user.type(within(form).getByLabelText("Peer ID"), "relay-1");
    await user.type(within(form).getByLabelText(/Server names/), "relay1.example");
    await user.click(within(form).getByRole("button", { name: "Create token" }));
    await screen.findByRole("region", { name: /^Token for relay-1/ });
    expect(screen.queryByRole("group", { name: "Install the agent" })).not.toBeInTheDocument();
  });

  it("preselects Relay from the relays page's link", async () => {
    window.history.replaceState(null, "", "/admin/tokens/?role=relay");
    stubFetch({ "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } }, "GET /api/sites": sites });
    render(<TokensAdmin me={customerAdmin} />);
    expect(await screen.findByRole("radio", { name: "Relay" })).toBeChecked();
    expect(screen.getByLabelText(/Server names/)).toBeEnabled();
  });

  it("hides used tokens until asked, and revokes a pending one in its row", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/tokens": [
        {
          status: 200,
          body: {
            tokens: [token(), token({ id: "t3", peer_id: "web-02", state: "used", used_at: "2026-10-22T10:30:00Z" })],
            next_cursor: null,
          },
        },
        { status: 200, body: { tokens: [], next_cursor: null } },
      ],
      "GET /api/sites": sites,
      "DELETE /api/tokens/t1": { status: 204 },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const pending = await screen.findByRole("row", { name: /web-01/ });
    expect(pending).toHaveTextContent(/web-01\s*Agent\s*paris\s*Pending/);
    expect(screen.getByRole("region", { name: "Tokens" })).toHaveTextContent("1 pending");
    expect(screen.queryByRole("row", { name: /web-02/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("checkbox", { name: "Hide used and expired" }));
    const used = screen.getByRole("row", { name: /web-02/ });
    expect(within(used).queryByRole("button")).not.toBeInTheDocument();
    expect(used).toHaveTextContent(/Used.*used /);

    await user.click(within(pending).getByRole("button", { name: "Revoke the token for web-01" }));
    const confirm = within(pending).getByRole("form", { name: "Revoke this token?" });
    expect(confirm).toHaveTextContent("web-01 will not be able to enroll with it.");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(false);
    await user.click(within(confirm).getByRole("button", { name: "Revoke token" }));
    expect(await screen.findByText("No tokens yet.")).toBeInTheDocument();
    const call = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "DELETE");
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
  });

  it("explains that site administrators cannot enroll", () => {
    const fetchMock = stubFetch({});
    render(<TokensAdmin me={siteAdmin} />);
    expect(screen.getByText(/site administrator role is not enough/)).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("builds the configuration snippet", () => {
    expect(configSnippet(created)).toBe(
      `upstream_url = "https://mf.example:7443"\nenroll_url = "https://mf.example:7445"\nenrollment_token = "${SECRET}"`,
    );
  });
});
