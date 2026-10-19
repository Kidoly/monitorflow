import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

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
    const form = await screen.findByRole("form", { name: "New enrollment token" });
    // Only sites of administered customers are offered.
    const site = within(form).getByLabelText("Site");
    expect(within(site).getAllByRole("option").map((o) => o.textContent)).toEqual(["acme / paris"]);
    await user.selectOptions(within(form).getByLabelText("Role"), "relay");
    await user.type(within(form).getByLabelText("Peer ID"), "relay-1");
    await user.selectOptions(within(form).getByLabelText("Valid for"), "86400");
    await user.type(within(form).getByLabelText(/Server names/), "relay1.example, 10.0.0.5");
    await user.click(within(form).getByRole("button", { name: "Create token" }));

    expect(await screen.findByLabelText("Enrollment token")).toHaveTextContent(SECRET);
    expect(screen.getByText(/shown only once/)).toBeInTheDocument();
    expect(screen.getByLabelText("Configuration")).toHaveTextContent(
      `upstream_url = "https://mf.example:7443" enroll_url = "https://mf.example:7445" enrollment_token = "${SECRET}"`,
    );
    const post = fetchMock.mock.calls.findIndex(([, init]) => init?.method === "POST");
    expect(requestHeaders(fetchMock, post)[CSRF_HEADER]).toBe("1");
    expect(JSON.parse(String(fetchMock.mock.calls[post]?.[1]?.body))).toEqual({
      role: "relay",
      peer_id: "relay-1",
      site_id: "site-1",
      ttl_s: 86400,
      server_names: ["relay1.example", "10.0.0.5"],
    });

    await user.click(screen.getByRole("button", { name: "Copy token" }));
    expect(await navigator.clipboard.readText()).toBe(SECRET);
    expect(screen.getByRole("status")).toHaveTextContent("Token copied.");

    // Once dismissed, nothing on the page shows the token again.
    await user.click(screen.getByRole("button", { name: "Done" }));
    expect(screen.queryByText(SECRET, { exact: false })).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain(SECRET);
    expect(await screen.findByRole("row", { name: /relay-1/ })).toBeInTheDocument();
  });

  it("revokes a pending token after confirmation", async () => {
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
    const used = await screen.findByRole("row", { name: /web-02/ });
    expect(within(used).queryByRole("button")).not.toBeInTheDocument();
    expect(used).toHaveTextContent("used");
    await userEvent.click(screen.getByRole("button", { name: "Revoke the token for web-01" }));
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "DELETE")).toBe(false);
    await userEvent.click(screen.getByRole("button", { name: "Confirm revoke" }));
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
