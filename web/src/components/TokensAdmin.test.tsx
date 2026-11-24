// @vitest-environment-options {"url": "https://mf.example/settings/add-servers/"}
import { render, screen, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import {
  type AgentInstall,
  CSRF_HEADER,
  type CreatedInstallKey,
  type CreatedToken,
  type EnrollmentToken,
  type InstallKey,
  type PendingHost,
  type PublicAddress,
} from "@/lib/api";
import { customerAdmin, me, siteAdmin } from "@/test/admin";
import { requestHeaders, stubFetch as stubRoutes } from "@/test/fetch";

import { hostsWords } from "./InstallKeys";
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

const address = (overrides: Partial<PublicAddress> = {}): PublicAddress => ({
  address: "mf.example",
  default: "mf.example",
  names: ["mf.example", "10.0.0.2"],
  certified: true,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  ...overrides,
});

const created: CreatedToken = {
  ...token({ id: "t2", role: "relay", peer_id: "relay-1", server_names: ["relay1.example"] }),
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  install: install(),
  address: address(),
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

const key = (overrides: Partial<InstallKey> = {}): InstallKey => ({
  id: "k1",
  role: "agent",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  created_by_username: "alice",
  created_at: "2026-10-22T10:00:00Z",
  expires_at: "2026-11-21T10:00:00Z",
  max_hosts: null,
  host_count: 0,
  require_approval: false,
  revoked_at: null,
  state: "active",
  ...overrides,
});

const pendingHost: PendingHost = {
  peer_id: "web-01-2",
  hostname: "web-01",
  site_id: "site-1",
  site_name: "paris",
  customer_name: "acme",
  install_key_id: "k1",
  source: "192.0.2.7",
  created_at: "2026-10-22T10:05:00Z",
};

/** Every page load also lists install keys; none unless a test says otherwise. */
const stubFetch = (routes: Parameters<typeof stubRoutes>[0]) =>
  stubRoutes({ "GET /api/install-keys": { status: 200, body: { keys: [], pending_hosts: [] } }, ...routes });

/** Opens the single-use tokens under Advanced and returns their form. */
async function newTokenForm(user: UserEvent) {
  await user.click(await screen.findByRole("button", { name: "Show single-use tokens" }));
  return screen.findByRole("form", { name: "New token" });
}

describe("TokensAdmin", () => {
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("leads with one install command for a site, shown once", async () => {
    const user = userEvent.setup();
    const createdKey: CreatedInstallKey = {
      ...key(),
      token: SECRET,
      upstream_url: "https://mf.example:7443",
      enroll_url: "https://mf.example:7445",
      install: install(),
      address: address(),
    };
    const fetchMock = stubFetch({
      "GET /api/install-keys": [
        { status: 200, body: { keys: [], pending_hosts: [] } },
        { status: 200, body: { keys: [key()], pending_hosts: [] } },
      ],
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/install-keys": { status: 201, body: createdKey },
    });
    render(<TokensAdmin me={customerAdmin} />);
    expect(await screen.findByText("None yet: create one above, then paste it on your servers.")).toBeInTheDocument();
    // Single-use tokens wait under Advanced.
    expect(screen.queryByRole("form", { name: "New token" })).not.toBeInTheDocument();
    // Its "How it works" sits in the section's heading line, not alone under it.
    const advanced = screen.getByRole("region", { name: "Advanced" });
    expect(within(advanced).getByRole("img", { name: "How it works" }).closest("div")).toHaveTextContent("Advanced");
    const form = screen.getByRole("form", { name: "New command" });
    // The page title's tooltip says how servers are named; the form says it no second time.
    expect(form).not.toHaveTextContent("appears under its own name");
    expect(within(form).getByText("(servers)")).toBeInTheDocument();
    await within(within(form).getByLabelText("Site")).findByRole("option");
    expect(within(form).getByLabelText("Works for")).toHaveValue("2592000");
    await user.click(within(form).getByRole("button", { name: "Create install command" }));

    const panel = await screen.findByRole("region", { name: /^Install command for paris · works until/ });
    expect(within(panel).getByRole("alert")).toHaveTextContent(
      "This command will not be shown again. Copy it now and keep it with your notes or deployment scripts. It works on any number of servers.",
    );
    const commands = within(panel).getByRole("group", { name: "Install the agent" });
    expect(within(commands).getByLabelText(/^Linux/)).toHaveTextContent(
      `| sudo MF_AI_COMMANDS_FROM_WEB=1 MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`,
    );
    expect(within(commands).getByLabelText(/^Windows/)).toHaveTextContent(`ENROLLMENT_TOKEN=${SECRET}`);
    const post = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "POST");
    expect(requestHeaders(fetchMock, post)[CSRF_HEADER]).toBe("1");
    expect(JSON.parse(String(fetchMock.mock.calls[post]?.[1]?.body))).toEqual({
      site_id: "site-1",
      ttl_s: 2592000,
      require_approval: false,
    });

    await user.click(within(panel).getByRole("button", { name: "Done" }));
    expect(document.body.textContent).not.toContain(SECRET);
    const row = await screen.findByRole("row", { name: /acme › paris/ });
    expect(row).toHaveTextContent(/0 servers\s*Active/);
    expect(screen.getByRole("form", { name: "New command" })).toBeInTheDocument();
  });

  it("sends a host limit and approval when asked", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/install-keys": { status: 400, body: { error: "the host limit must be between 1 and 100000" } },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await screen.findByRole("form", { name: "New command" });
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.selectOptions(within(form).getByLabelText("Works for"), "604800");
    await user.type(within(form).getByLabelText(/^Up to/), "10");
    await user.click(within(form).getByRole("switch", { name: "Approve new servers before they appear" }));
    await user.click(within(form).getByRole("button", { name: "Create install command" }));
    expect(await within(form).findByRole("alert")).toHaveTextContent("Creating the install command failed.");
    const post = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(fetchMock.mock.calls[post]?.[1]?.body))).toEqual({
      site_id: "site-1",
      ttl_s: 604800,
      max_hosts: 10,
      require_approval: true,
    });
  });

  it("approves or rejects waiting hosts and revokes a command", async () => {
    const user = userEvent.setup();
    const approvalKey = key({ require_approval: true, max_hosts: 5, host_count: 2 });
    const fetchMock = stubFetch({
      "GET /api/install-keys": [
        { status: 200, body: { keys: [approvalKey], pending_hosts: [pendingHost, { ...pendingHost, peer_id: "db-1", hostname: "db-1" }] } },
        { status: 200, body: { keys: [approvalKey], pending_hosts: [{ ...pendingHost, peer_id: "db-1", hostname: "db-1" }] } },
        { status: 200, body: { keys: [approvalKey], pending_hosts: [] } },
        { status: 200, body: { keys: [{ ...approvalKey, state: "revoked" }], pending_hosts: [] } },
      ],
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/pending-hosts/web-01-2/approve": { status: 204 },
      "POST /api/pending-hosts/db-1/reject": { status: 204 },
      "DELETE /api/install-keys/k1": { status: 204 },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const waiting = await screen.findByRole("region", { name: "Waiting for approval" });
    const web = within(waiting).getByRole("row", { name: /web-01/ });
    expect(web).toHaveTextContent(/web-01 · named web-01-2\s*paris\s*192\.0\.2\.7/);
    await user.click(within(web).getByRole("button", { name: "Approve web-01" }));
    await screen.findByRole("button", { name: "Reject db-1" }).then((b) => user.click(b));
    const confirm = screen.getByRole("form", { name: "Reject this server?" });
    expect(confirm).toHaveTextContent("it can never connect as db-1");
    await user.click(within(confirm).getByRole("button", { name: "Reject server" }));
    await screen.findByRole("region", { name: "Commands created" });
    expect(screen.queryByRole("region", { name: "Waiting for approval" })).not.toBeInTheDocument();

    const row = screen.getByRole("row", { name: /acme › paris/ });
    expect(row).toHaveTextContent("2 of 5 servers · with approval");
    await user.click(within(row).getByRole("button", { name: "Revoke the install command of paris" }));
    await user.click(within(row).getByRole("button", { name: "Revoke command" }));
    expect(await screen.findByText("Revoked")).toBeInTheDocument();
    const methods = fetchMock.mock.calls.map(([url, init]) => `${init?.method ?? "GET"} ${String(url)}`);
    expect(methods).toEqual(
      expect.arrayContaining([
        "POST /api/pending-hosts/web-01-2/approve",
        "POST /api/pending-hosts/db-1/reject",
        "DELETE /api/install-keys/k1",
      ]),
    );
  });

  it("words host counts", () => {
    expect(hostsWords({ host_count: 0, max_hosts: null })).toBe("0 servers");
    expect(hostsWords({ host_count: 1, max_hosts: null })).toBe("1 server");
    expect(hostsWords({ host_count: 1, max_hosts: 1 })).toBe("1 of 1 server");
    expect(hostsWords({ host_count: 3, max_hosts: 10 })).toBe("3 of 10 servers");
    setActiveLocale("fr");
    expect(hostsWords({ host_count: 0, max_hosts: null })).toBe("0 serveur");
    expect(hostsWords({ host_count: 3, max_hosts: 10 })).toBe("3 sur 10 serveurs");
  });

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
    const form = await newTokenForm(user);
    expect(form).not.toHaveTextContent("audit log");
    // Only sites of administered customers are offered.
    const site = within(form).getByLabelText("Site");
    await within(site).findByRole("option");
    expect(within(site).getAllByRole("option").map((o) => o.textContent)).toEqual(["acme › paris"]);
    // Server names are for relays only.
    expect(within(form).getByLabelText(/Addresses/)).toBeDisabled();
    await user.click(within(form).getByRole("radio", { name: "Relay" }));
    await user.type(within(form).getByLabelText("Name"), "relay-1");
    await user.selectOptions(within(form).getByLabelText("Expires in"), "86400");
    await user.type(within(form).getByLabelText(/Addresses/), "relay1.example, 10.0.0.5");
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
    await user.click(within(install).getByRole("switch", { name: /package mirrors/ }));
    expect(within(install).getByLabelText(/^Linux/)).toHaveTextContent("MF_ROLE=relay MF_MIRROR=1 MF_SERVER=");
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
    const form = await newTokenForm(user);
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Name"), "web-03");
    await user.click(within(form).getByRole("button", { name: "Create token" }));

    const install = await screen.findByRole("group", { name: "Install the agent" });
    const linux = within(install).getByLabelText(/^Linux/);
    expect(linux).toHaveTextContent(`| sudo MF_AI_COMMANDS_FROM_WEB=1 MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`);
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
    expect(within(install).getByRole("alert")).toHaveTextContent("Enter a name or IP address");
    expect(linux).toHaveTextContent("MF_SERVER=mf.example:7443 MF_TOKEN");
    expect(linux).not.toHaveTextContent("; id");

    // On by default (ADR-0175): both lines let the web interface turn AI commands on (ADR-0098); unticked, neither does.
    const fromWeb = within(install).getByRole("switch", { name: /Allow AI commands to be turned on from the web interface/ });
    expect(fromWeb).toBeChecked();
    expect(linux).toHaveTextContent("sudo MF_AI_COMMANDS_FROM_WEB=1 MF_SERVER=");
    expect(within(install).getByLabelText(/^Windows/)).toHaveTextContent("AI_COMMANDS_FROM_WEB=1");
    await user.click(fromWeb);
    expect(linux).not.toHaveTextContent("MF_AI_COMMANDS_FROM_WEB");
    expect(within(install).getByLabelText(/^Windows/)).not.toHaveTextContent("AI_COMMANDS_FROM_WEB");

    // Off by default (ADR-0189): scripts need the host's own choice.
    const scripts = within(install).getByRole("switch", { name: /Allow scripts from the script library/ });
    expect(scripts).not.toBeChecked();
    expect(linux).not.toHaveTextContent("MF_ALLOW_SCRIPTS");
    await user.click(scripts);
    expect(linux).toHaveTextContent("sudo MF_ALLOW_SCRIPTS=1 MF_SERVER=");
    expect(within(install).getByLabelText(/^Windows/)).toHaveTextContent("ALLOW_SCRIPTS=1");
  });

  it("lets global administrators change the address of every command in one step", async () => {
    const user = userEvent.setup();
    const agentToken: CreatedToken = { ...created, ...token({ id: "t7", peer_id: "web-06" }) };
    const fetchMock = stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/tokens": { status: 201, body: agentToken },
      "PUT /api/settings/public-address": [
        {
          status: 200,
          body: address({
            address: "10.0.0.2",
            upstream_url: "https://10.0.0.2:7443",
            enroll_url: "https://10.0.0.2:7445",
          }),
        },
        {
          status: 200,
          body: address({
            address: "monitor.lan",
            certified: false,
            upstream_url: "https://monitor.lan:7443",
            enroll_url: "https://monitor.lan:7445",
          }),
        },
      ],
    });
    render(<TokensAdmin me={globalAdmin} />);
    const form = await newTokenForm(user);
    await within(within(form).getByLabelText("Site")).findAllByRole("option");
    await user.type(within(form).getByLabelText("Name"), "web-06");
    await user.click(within(form).getByRole("button", { name: "Create token" }));

    const panel = await screen.findByRole("region", { name: /^Token for web-06/ });
    const picker = within(panel).getByRole("combobox", { name: /Agents reach this server at/ });
    // The certificate's names; this page's host, mf.example, is one of them.
    expect(within(picker).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "mf.example (default)",
      "10.0.0.2",
      "Other address…",
    ]);
    await user.selectOptions(picker, "10.0.0.2");
    const linux = within(panel).getByLabelText(/^Linux/);
    expect(await within(panel).findByText(/MF_SERVER=10\.0\.0\.2:7443/)).toBeInTheDocument();
    expect(within(panel).getByLabelText(/^Windows/)).toHaveTextContent("UPSTREAM_URL=https://10.0.0.2:7443");
    expect(within(panel).getByLabelText(/Configuration snippet/)).toHaveTextContent(
      'upstream_url = "https://10.0.0.2:7443" enroll_url = "https://10.0.0.2:7445"',
    );
    const put = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(fetchMock.mock.calls[put]?.[1]?.body))).toEqual({ address: "10.0.0.2" });
    expect(requestHeaders(fetchMock, put)[CSRF_HEADER]).toBe("1");
    expect(within(panel).queryByText(/will refuse to connect/)).not.toBeInTheDocument();

    // A typed address applies too, with a plain warning when the certificate lacks it.
    await user.selectOptions(picker, "Other address…");
    await user.type(within(panel).getByRole("textbox", { name: "Other address" }), "monitor.lan");
    await user.click(within(panel).getByRole("button", { name: "Use this address" }));
    expect(await within(panel).findByText(/Agents will refuse to connect to monitor\.lan/)).toBeInTheDocument();
    expect(panel).toHaveTextContent("its names: mf.example, 10.0.0.2");
    expect(linux).toHaveTextContent("MF_SERVER=monitor.lan:7443");
    expect(picker).toHaveValue("monitor.lan");
    expect(within(panel).queryByRole("textbox", { name: "Other address" })).not.toBeInTheDocument();
  });

  it("shows the address read-only to customer administrators", async () => {
    const user = userEvent.setup();
    const agentToken: CreatedToken = {
      ...created,
      ...token({ id: "t8", peer_id: "web-07" }),
      address: address({ address: "monitor.lan", certified: false }),
    };
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
      "POST /api/tokens": { status: 201, body: agentToken },
    });
    render(<TokensAdmin me={customerAdmin} />);
    const form = await newTokenForm(user);
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Name"), "web-07");
    await user.click(within(form).getByRole("button", { name: "Create token" }));
    const panel = await screen.findByRole("region", { name: /^Token for web-07/ });
    expect(within(panel).queryByRole("combobox", { name: /Agents reach/ })).not.toBeInTheDocument();
    expect(panel).toHaveTextContent("monitor.lan. A global administrator changes it.");
    expect(panel).toHaveTextContent("Agents will refuse to connect to monitor.lan");
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
    const form = await newTokenForm(user);
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Name"), "web-04");
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
    const form = await newTokenForm(user);
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.type(within(form).getByLabelText("Name"), "web-05");
    await user.click(within(form).getByRole("button", { name: "Create token" }));
    const group = await screen.findByRole("group", { name: "Install the agent" });
    expect(group).toHaveTextContent(
      "This server holds no agent release yet. A global administrator imports one under Settings › Server update.",
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
    // The URL field shows only for a custom source.
    expect(within(section).queryByRole("textbox", { name: /^URL/ })).not.toBeInTheDocument();
    await user.click(within(section).getByRole("radio", { name: "Custom URL" }));
    const save = within(section).getByRole("button", { name: "Save" });
    expect(save).toBeDisabled();
    const url = within(section).getByRole("textbox", { name: /^URL/ });
    expect(url).toHaveAttribute("placeholder", "https://files.example.com/monitorflow/");
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
    expect(section).toHaveTextContent("set in the server configuration: agent_install_source");
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
    const form = await newTokenForm(user);
    await within(within(form).getByLabelText("Site")).findByRole("option");
    await user.click(within(form).getByRole("radio", { name: "Relay" }));
    await user.type(within(form).getByLabelText("Name"), "relay-1");
    await user.type(within(form).getByLabelText(/Addresses/), "relay1.example");
    await user.click(within(form).getByRole("button", { name: "Create token" }));
    await screen.findByRole("region", { name: /^Token for relay-1/ });
    expect(screen.queryByRole("group", { name: "Install the agent" })).not.toBeInTheDocument();
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
    await user.click(await screen.findByRole("button", { name: "Show single-use tokens" }));
    const pending = await screen.findByRole("row", { name: /web-01/ });
    expect(pending).toHaveTextContent(/web-01\s*Agent\s*paris\s*Pending/);
    expect(screen.getByRole("region", { name: "Tokens" })).toHaveTextContent("1 pending");
    expect(screen.queryByRole("row", { name: /web-02/ })).not.toBeInTheDocument();

    await user.click(screen.getByRole("switch", { name: "Hide used and expired" }));
    const used = screen.getByRole("row", { name: /web-02/ });
    expect(within(used).queryByRole("button")).not.toBeInTheDocument();
    expect(used).toHaveTextContent(/Used.*used /);

    await user.click(within(pending).getByRole("button", { name: "Revoke the token for web-01" }));
    const confirm = within(pending).getByRole("form", { name: "Revoke this token?" });
    expect(confirm).toHaveTextContent("web-01 will not be able to connect with it.");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(false);
    await user.click(within(confirm).getByRole("button", { name: "Revoke token" }));
    expect(await screen.findByText("No tokens yet.")).toBeInTheDocument();
    const call = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "DELETE");
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
  });

  it("speaks French", async () => {
    setActiveLocale("fr");
    const user = userEvent.setup();
    stubFetch({
      "GET /api/tokens": { status: 200, body: { tokens: [token()], next_cursor: null } },
      "GET /api/sites": sites,
      "GET /api/settings/agent-install": settings,
    });
    render(<TokensAdmin me={customerAdmin} />);
    expect(screen.getByRole("heading", { level: 1, name: "Ajouter des serveurs" })).toBeInTheDocument();
    await user.click(await screen.findByRole("button", { name: "Afficher les jetons à usage unique" }));
    expect(await screen.findByRole("form", { name: "Nouveau jeton" })).toBeInTheDocument();
    expect(screen.getByRole("region", { name: "Jetons" })).toHaveTextContent("1 en attente");
    expect(screen.getByRole("button", { name: "Révoquer le jeton de web-01" })).toBeInTheDocument();
    expect(await screen.findByText("Source d’installation par défaut")).toBeInTheDocument();
    expect(screen.getByRole("form", { name: "Nouvelle commande" })).toHaveTextContent("Approuver les nouveaux serveurs");
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
