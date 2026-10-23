import { render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { As, customerAdmin, relay, viewer } from "@/test/admin";
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
        body: { relays: [relay({ id: "r2", peer_id: "relay-2", status: "offline", cert_not_after: null })], next_cursor: null },
      },
    });
    render(
      <As user={customerAdmin}>
        <RelayListPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Relays" });
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(/paris \(acme\)\s*relay-1\s*● Online\s*1 connected, 2 attached\s*relay1\.paris\.example/);
    expect(rows[1]).toHaveTextContent("⚠ Expires in 5 days");
    expect(within(rows[1]!).getByRole("link", { name: "relay-1" })).toHaveAttribute(
      "href",
      expect.stringMatching(/^\/admin\/relays\/view\/?\?id=6f1c2a52-3b5e-4c43-9a51-0e7f6d1f1a02$/),
    );
    expect(rows[2]).toHaveTextContent(/relay-2\s*○ Offline/);
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
