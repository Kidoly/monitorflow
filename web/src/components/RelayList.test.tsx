import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { As, customerAdmin, relay, siteAdmin, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { RelayListPage } from "./RelayList";

describe("RelayListPage", () => {
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
    expect(screen.getByRole("link", { name: "New relay token" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/admin\/tokens\/?\?role=relay$/),
    );
  });

  it("offers a relay token only to customer administrators", async () => {
    stubFetch({ "GET /api/relays": { status: 200, body: { relays: [relay()], next_cursor: null } } });
    render(
      <As user={siteAdmin}>
        <RelayListPage />
      </As>,
    );
    await screen.findByRole("link", { name: "relay-1" });
    expect(screen.queryByRole("link", { name: "New relay token" })).not.toBeInTheDocument();
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
