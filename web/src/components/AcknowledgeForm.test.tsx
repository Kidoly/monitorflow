import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it } from "vitest";

import type { AcknowledgeLink } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { AcknowledgeForm } from "./AcknowledgeForm";

const TOKEN = "ab".repeat(32);
const LINK: AcknowledgeLink = {
  username: "boss",
  expires_at: "2026-11-02T15:00:00Z",
  used_at: null,
  incidents: [
    { id: "i1", text: "Critical: web-1 (paris) is offline", state: "open", acknowledged_by: null, acknowledged_at: null },
    { id: "i2", text: "Critical: web-2 (paris) is offline", state: "resolved", acknowledged_by: null, acknowledged_at: null },
  ],
};
const INSPECT = { "POST /api/acknowledge-link": { status: 200, body: LINK } };

describe("AcknowledgeForm", () => {
  beforeEach(() => window.history.replaceState(null, "", `/acknowledge/#${TOKEN}`));
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("shows what it acknowledges, and acknowledges only on the button", async () => {
    const fetchMock = stubFetch({
      ...INSPECT,
      "POST /api/acknowledge-link/use": {
        status: 200,
        body: { ...LINK, incidents: [{ ...LINK.incidents[0], acknowledged_by: "boss", acknowledged_at: "2026-11-02T03:12:00Z" }] },
      },
    });
    render(<AcknowledgeForm />);
    expect(await screen.findByRole("heading", { name: "Acknowledge as boss" })).toBeInTheDocument();
    expect(screen.getByText("Critical: web-1 (paris) is offline")).toBeInTheDocument();
    expect(screen.getByText("Critical: web-2 (paris) is offline (resolved)")).toBeInTheDocument();
    // The secret leaves the address bar, and opening the page changed nothing.
    expect(window.location.hash).toBe("");
    expect(fetchMock.mock.calls.map(([url]) => url)).toEqual(["/api/acknowledge-link"]);

    await userEvent.setup().click(screen.getByRole("button", { name: "Acknowledge" }));
    expect(await screen.findByText(/^Acknowledged at \d\d:\d\d\. Nobody else is told next\.$/)).toBeInTheDocument();
    expect(screen.getByText(/acknowledged by boss at/)).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url]) => url === "/api/acknowledge-link/use");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ token: TOKEN });
  });

  it("says when the link no longer works", async () => {
    stubFetch({ "POST /api/acknowledge-link": { status: 410, body: { error: "expired" } } });
    render(<AcknowledgeForm />);
    expect(await screen.findByText("This link no longer works.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Sign in" })).toHaveAttribute("href", expect.stringMatching(/^\/login/));
  });

  it("asks nothing without a link", async () => {
    window.history.replaceState(null, "", "/acknowledge/");
    const fetchMock = stubFetch({});
    render(<AcknowledgeForm />);
    expect(await screen.findByText("This link no longer works.")).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("says when the account can no longer acknowledge", async () => {
    stubFetch({ ...INSPECT, "POST /api/acknowledge-link/use": { status: 403, body: { error: "forbidden" } } });
    render(<AcknowledgeForm />);
    await userEvent.setup().click(await screen.findByRole("button", { name: "Acknowledge" }));
    expect(await screen.findByText("Your account can no longer acknowledge these alerts.")).toBeInTheDocument();
  });
});
