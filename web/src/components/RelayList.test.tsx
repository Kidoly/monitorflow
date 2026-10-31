import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import type { AgentInstall, CreatedInstallKey } from "@/lib/api";
import { As, customerAdmin, relay, siteAdmin, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { RelayListPage } from "./RelayList";

const SECRET = "mfe1.c2VjcmV0.Y2E";
const install: AgentInstall = {
  source: "github",
  source_from_config: false,
  custom_url: null,
  custom_url_from_config: false,
  github_url: "https://github.com/Kidoly/monitorflow/releases/latest/download",
  central: null,
};
const relayKey: CreatedInstallKey = {
  id: "k1",
  role: "relay",
  site_id: "site-1",
  site_name: "paris",
  customer_id: "c1",
  customer_name: "acme",
  created_by_username: "alice",
  created_at: "2026-10-22T10:00:00Z",
  expires_at: "2026-10-23T10:00:00Z",
  max_hosts: 1,
  host_count: 0,
  require_approval: false,
  revoked_at: null,
  state: "active",
  token: SECRET,
  upstream_url: "https://mf.example:7443",
  enroll_url: "https://mf.example:7445",
  install,
  address: {
    address: "mf.example",
    default: "mf.example",
    names: ["mf.example"],
    certified: true,
    upstream_url: "https://mf.example:7443",
    enroll_url: "https://mf.example:7445",
  },
};
const sites = {
  status: 200,
  body: {
    sites: [{ id: "site-1", name: "paris", server_count: 0, relay_count: 0, customer_id: "c1", customer_name: "acme" }],
  },
};
const waiting = relay({
  server_names: [],
  pending_names: ["relay1.paris.example", "10.0.0.5"],
  pending_source: "203.0.113.9",
});

describe("RelayListPage", () => {
  it("adds a relay from the empty page with one command and a mirror checkbox", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/relays": { status: 200, body: { relays: [], next_cursor: null } },
      "GET /api/sites": sites,
      "POST /api/install-keys": { status: 201, body: relayKey },
    });
    render(
      <As user={customerAdmin}>
        <RelayListPage />
      </As>,
    );
    await screen.findByText("No relays yet");
    await user.click(screen.getAllByRole("button", { name: "Add relay" })[0]!);
    await screen.findByRole("option", { name: "acme › paris" });
    expect(screen.queryByLabelText(/Server names/)).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Show the install command" }));

    const command = await screen.findByLabelText(/^Linux/);
    expect(command).toHaveTextContent(`sudo MF_ROLE=relay MF_SERVER=mf.example:7443 MF_TOKEN=${SECRET} sh`);
    await user.click(screen.getByRole("checkbox", { name: /package mirrors/ }));
    expect(command).toHaveTextContent("MF_ROLE=relay MF_MIRROR=1 MF_SERVER=");
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({ role: "relay", site_id: "site-1", require_approval: false });
  });

  it("confirms the names a relay reported, then offers to add servers behind it", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      "GET /api/relays": { status: 200, body: { relays: [waiting], next_cursor: null } },
      [`POST /api/relays/${waiting.id}/names`]: { status: 204 },
    });
    render(
      <As user={customerAdmin}>
        <RelayListPage />
      </As>,
    );
    const panel = (await screen.findByRole("form", { name: "Confirm relay-1" })) as HTMLElement;
    expect(panel).toHaveTextContent("203.0.113.9");
    expect(screen.getByRole("table", { name: "Relays" })).toHaveTextContent("Waiting for confirmation");
    await user.click(within(panel).getByRole("checkbox", { name: "10.0.0.5" }));
    await user.type(within(panel).getByLabelText(/Another name/), "relay.lan");
    await user.click(within(panel).getByRole("button", { name: "Confirm names" }));

    const add = await screen.findByRole("link", { name: "Add servers behind this relay" });
    expect(add).toHaveAttribute("href", expect.stringContaining(waiting.id));
    const post = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(post?.[1]?.body))).toEqual({ names: ["relay1.paris.example", "relay.lan"] });
  });

  it("says why names were refused and keeps the form", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/relays": { status: 200, body: { relays: [waiting], next_cursor: null } },
      [`POST /api/relays/${waiting.id}/names`]: {
        status: 409,
        body: { error: "`relay1.paris.example` is this server's name: a relay cannot have it" },
      },
    });
    render(
      <As user={customerAdmin}>
        <RelayListPage />
      </As>,
    );
    await user.click(await screen.findByRole("button", { name: "Confirm names" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/Confirming the names failed/);
    expect(screen.getByRole("form", { name: "Confirm relay-1" })).toBeInTheDocument();
  });

  it("lists relays with status, agents and a certificate warning", async () => {
    stubFetch({
      "GET /api/relays": {
        status: 200,
        body: { relays: [relay()], next_cursor: "next" },
      },
      "GET /api/relays?cursor=next": {
        status: 200,
        body: {
          relays: [
            relay({ id: "r2", peer_id: "relay-2", status: "offline", cert_not_after: "2027-02-11T10:00:00Z" }),
            relay({ id: "r3", peer_id: "relay-3", status: "offline", cert_not_after: null }),
          ],
          next_cursor: null,
        },
      },
    });
    render(
      <As user={customerAdmin}>
        <RelayListPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Relays" });
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(4);
    expect(rows[1]).toHaveTextContent(/relay-1\s*paris · acme\s*Online\s*1 connected \/ 2 total\s*relay1\.paris\.example/);
    expect(within(rows[1]!).getByText("Expires in 5 days")).toBeInTheDocument();
    expect(within(rows[1]!).getByRole("link", { name: "relay-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/admin\/relays\/view\/?\?id=6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02$/),
    );
    expect(rows[2]).toHaveTextContent(/relay-2.*Offline.*Valid · 2027-02-11/);
    expect(rows[3]).toHaveTextContent(/relay-3.*Offline.*None/);
    expect(screen.getByText(/3 relays, 1 online/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add relay" })).toBeInTheDocument();
  });

  it("offers to add a relay only to customer administrators", async () => {
    stubFetch({ "GET /api/relays": { status: 200, body: { relays: [relay()], next_cursor: null } } });
    render(
      <As user={siteAdmin}>
        <RelayListPage />
      </As>,
    );
    await screen.findByRole("link", { name: "relay-1" });
    expect(screen.queryByRole("button", { name: "Add relay" })).not.toBeInTheDocument();
  });

  it("is only shown to administrators", () => {
    const fetchMock = stubFetch({});
    render(
      <As user={viewer}>
        <RelayListPage />
      </As>,
    );
    expect(screen.getByText(/needs an administrator role/)).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
