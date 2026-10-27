// @vitest-environment-options {"url": "https://mf.example/admin/tokens/"}
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { type AgentInstall, CSRF_HEADER, type CreatedToken, type EnrollmentToken } from "@/lib/api";
import { customerAdmin, me, siteAdmin } from "@/test/admin";
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

const SUMS = "a".repeat(64);
const INSTALLER = "b".repeat(64);

const install = (overrides: Partial<AgentInstall> = {}): AgentInstall => ({
  source: "github",
  custom_url: null,
  source_from_config: false,
  custom_url_from_config: false,
  github_url: "https://github.com/Kidoly/monitorflow/releases/latest/download",
  central: {
    version: "0.3.0",
    path: "/agent/0.3.0",
    sums_sha256: SUMS,
    installer_sha256: INSTALLER,
    msi_sha256: "c".repeat(64),
  },
  ...overrides,
});

const created: CreatedToken = {
  ...token({ id: "t2", role: "relay", peer_id: "relay-1", server_names: ["relay1.example"] }),
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  install: install(),
};

const settings = { status: 200, body: install() };
const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });

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
      "GET /api/settings/agent-install": settings,
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
    expect(alert.compareDocumentPosition(within(panel).getByRole("textbox", { name: "Token" }))).toBe(
      Node.DOCUMENT_POSITION_FOLLOWING,
    );
    expect(within(panel).getByLabelText(/Configuration snippet/)).toHaveTextContent(
      `upstream_url = "https://mf.example:7443" enroll_url = "https://mf.example:7445" enrollment_token = "${SECRET}"`,
    );
    expect(panel).toHaveTextContent("/etc/monitorflow/relay.toml");
    // The relay's one-liner, from the default source.
    const install = within(panel).getByRole("group", { name: "Install the relay" });
    expect(within(install).getByLabelText(/^Linux/)).toHaveTextContent(
      `| sudo MF_ROLE=relay MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`,
    );
    await user.type(within(install).getByLabelText(/Serve mirrors at/), "http://relay1.example:7480");
    expect(within(install).getByLabelText(/^Linux/)).toHaveTextContent("MF_MIRROR_URL=http://relay1.example:7480");
    expect(within(panel).queryByRole("group", { name: "Install the agent" })).not.toBeInTheDocument();
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
      "GET /api/settings/agent-install": settings,
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

  it("follows the chosen source, preselected with the default", async () => {
    const user = userEvent.setup();
    const agentToken: CreatedToken = {
      ...created,
      ...token({ id: "t5", peer_id: "web-04" }),
      install: install({ source: "central" }),
    };
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/tokens": { status: 201, body: agentToken },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New token" });
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Peer ID"), "web-04");
    await user.click(within(form).getByRole("button", { name: "Create token" }));

    const group = await screen.findByRole("group", { name: "Install the agent" });
    expect(within(group).getByRole("radio", { name: "This server" })).toBeChecked();
    expect(group).toHaveTextContent("Release 0.3.0 from this server");
    const linux = within(group).getByLabelText(/^Linux/);
    expect(linux).toHaveTextContent("curl -fsSLk -o \"$f\" https://mf.example/agent/0.3.0/install.sh");
    expect(linux.textContent).toContain(`echo "${INSTALLER}  $f" | sha256sum -c`);
    expect(linux).toHaveTextContent(`MF_DOWNLOAD_URL=https://mf.example/agent/0.3.0 MF_SUMS_SHA256=${SUMS} MF_SERVER=`);
    expect(within(group).getByLabelText(/^Windows/)).toHaveTextContent("curl.exe -fsSLk");
    // The relay field applies to every source.
    await user.type(within(group).getByLabelText(/Connect through a relay/), "relay1:7443");
    expect(linux).toHaveTextContent("MF_SERVER=relay1:7443 MF_ENROLL=mf.example:7445");

    await user.click(within(group).getByRole("radio", { name: "GitHub" }));
    expect(within(group).getByLabelText(/^Linux/)).toHaveTextContent(
      "curl -fsSL https://github.com/Kidoly/monitorflow/releases/latest/download/install.sh",
    );
    await user.click(within(group).getByRole("radio", { name: "Custom URL" }));
    expect(group).toHaveTextContent("No custom URL is set");
    expect(within(group).queryByLabelText(/^Linux/)).not.toBeInTheDocument();
  });

  it("says when the server holds no release to install from", async () => {
    const user = userEvent.setup();
    const agentToken: CreatedToken = {
      ...created,
      ...token({ id: "t6", peer_id: "web-05" }),
      install: install({ source: "central", central: null }),
    };
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/tokens": { status: 201, body: agentToken },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New token" });
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Peer ID"), "web-05");
    await user.click(within(form).getByRole("button", { name: "Create token" }));
    const group = await screen.findByRole("group", { name: "Install the agent" });
    expect(group).toHaveTextContent(
      "This server holds no agent release yet. An administrator imports one on the server with `monitorflow-server agent-release import --dir DIR`.",
    );
    expect(within(group).queryByLabelText(/^Linux/)).not.toBeInTheDocument();
    expect(within(group).queryByLabelText(/^Windows/)).not.toBeInTheDocument();
  });

  it("lets global administrators change the default source", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "PUT /api/settings/agent-install": {
        status: 200,
        body: install({ source: "custom", custom_url: "https://files.example/mf" }),
      },
    });
    render(<TokensAdmin me={globalAdmin} />);
    const section = await screen.findByRole("form", { name: "Default install source" });
    expect(await within(section).findByRole("radio", { name: "GitHub" })).toBeChecked();
    expect(section).toHaveTextContent("This server holds release 0.3.0.");
    await user.click(within(section).getByRole("radio", { name: "Custom URL" }));
    const save = within(section).getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    const url = within(section).getByRole("textbox", { name: /^Custom URL/ });
    await user.type(url, "https://files.example/$(id)");
    expect(url).toHaveAttribute("aria-invalid", "true");
    expect(save).toBeDisabled();
    await user.clear(url);
    await user.type(url, "https://files.example/mf/");
    await user.click(save);
    expect(await within(section).findByText("Saved.")).toBeInTheDocument();
    const call = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(fetchMock.mock.calls[call]?.[1]?.body))).toEqual({
      source: "custom",
      custom_url: "https://files.example/mf/",
    });
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
    expect(within(section).getByRole("radio", { name: "Custom URL" })).toBeChecked();
  });

  it("shows the default source read-only to others, and what the configuration sets", async () => {
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": { status: 200, body: install({ source_from_config: true, central: null }) },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const section = await screen.findByRole("form", { name: "Default install source" });
    expect(await within(section).findByRole("radio", { name: "GitHub" })).toBeDisabled();
    expect(section).toHaveTextContent("A global administrator changes it.");
    expect(section).toHaveTextContent("set in the server configuration (agent_install_source)");
    expect(section).toHaveTextContent("This server holds no agent release yet");
    expect(within(section).queryByRole("button", { name: "Save" })).not.toBeInTheDocument();
  });

  it("shows no install commands for a relay token", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
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
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
    });
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
      "GET /api/settings/agent-install": settings,
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
