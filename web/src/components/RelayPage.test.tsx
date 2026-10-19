import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { CSRF_HEADER, type RelayDetail } from "@/lib/api";
import { As, customerAdmin, relay, siteAdmin } from "@/test/admin";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { RelayPage } from "./RelayPage";

const detail = (overrides: Partial<RelayDetail> = {}): RelayDetail => ({
  ...relay(),
  certificate: null,
  servers: [
    { id: "6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a03", peer_id: "web-1", hostname: "web-1", status: "online" },
  ],
  ...overrides,
});
const ID = relay().id;

describe("RelayPage", () => {
  it("shows the relay and links its agents to their server pages", async () => {
    stubFetch({ [`GET /api/relays/${ID}`]: { status: 200, body: detail() } });
    render(<RelayPage id={ID} me={siteAdmin} />);
    expect(await screen.findByRole("heading", { level: 1, name: "relay-1" })).toBeInTheDocument();
    expect(screen.getByText(/Expires/)).toBeInTheDocument();
    const agents = screen.getByRole("region", { name: "Attached agents" });
    expect(within(agents).getByRole("link", { name: "web-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/servers\/view\/?\?id=6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a03$/),
    );
    // A site administrator cannot revoke.
    expect(screen.queryByRole("button", { name: /Revoke/ })).not.toBeInTheDocument();
  });

  it("revokes after confirmation, with the CSRF header and the reason", async () => {
    const fetchMock = stubFetch({
      [`GET /api/relays/${ID}`]: [
        { status: 200, body: detail() },
        { status: 200, body: detail({ revoked_at: "2026-10-22T10:00:00Z", status: "offline" }) },
      ],
      [`POST /api/relays/${ID}/revoke`]: {
        status: 200,
        body: { revoked_at: "2026-10-22T10:00:00Z", reason: "key_compromise", serials: ["0a"], closed_streams: 2 },
      },
    });
    render(
      <As user={customerAdmin}>
        <RelayPage id={ID} me={customerAdmin} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Revoke relay…" }));
    const form = screen.getByRole("form", { name: "Revoke relay-1" });
    expect(form).toHaveTextContent("including the agents it forwards");
    await userEvent.click(within(form).getByRole("button", { name: "Cancel" }));
    expect(fetchMock).toHaveBeenCalledTimes(1);

    await userEvent.click(screen.getByRole("button", { name: "Revoke relay…" }));
    await userEvent.selectOptions(screen.getByLabelText("Reason"), "key_compromise");
    await userEvent.click(screen.getByRole("button", { name: "Revoke relay-1" }));
    expect(await screen.findByText(/Revoked/)).toBeInTheDocument();
    const call = fetchMock.mock.calls.findIndex(([url]) => String(url).endsWith("/revoke"));
    expect(requestHeaders(fetchMock, call)[CSRF_HEADER]).toBe("1");
    expect(fetchMock.mock.calls[call]?.[1]?.body).toBe(JSON.stringify({ reason: "key_compromise" }));
    expect(screen.queryByRole("button", { name: /Revoke/ })).not.toBeInTheDocument();
  });

  it("says when the relay is not found", async () => {
    stubFetch({ [`GET /api/relays/${ID}`]: { status: 404, body: { error: "not found" } } });
    render(<RelayPage id={ID} me={customerAdmin} />);
    expect(await screen.findByRole("heading", { name: "Relay not found" })).toBeInTheDocument();
  });
});
