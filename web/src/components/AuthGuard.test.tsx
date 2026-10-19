import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { CSRF_HEADER, LOGIN_PATH } from "@/lib/api";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { AuthGuard } from "./AuthGuard";

const replace = vi.fn();
// Like Next's, the router object is stable across renders.
const router = { replace };
vi.mock("next/navigation", () => ({ useRouter: () => router }));

const alice = { id: "u1", username: "alice", roles: [{ role: "viewer", site_id: null }] };

describe("AuthGuard", () => {
  beforeEach(() => replace.mockReset());

  it("redirects to the login page on 401 without rendering the page", async () => {
    stubFetch({ "GET /api/auth/me": { status: 401, body: { error: "authentication required" } } });
    render(<AuthGuard>secret page</AuthGuard>);
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith(LOGIN_PATH));
    expect(screen.queryByText("secret page")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows the current user and the page", async () => {
    stubFetch({ "GET /api/auth/me": { status: 200, body: alice } });
    render(<AuthGuard>fleet page</AuthGuard>);
    expect(await screen.findByText("fleet page")).toBeInTheDocument();
    expect(screen.getByRole("banner")).toHaveTextContent("Signed in as alice (viewer)");
  });

  it("shows Administration to administrators only", async () => {
    stubFetch({ "GET /api/auth/me": { status: 200, body: alice } });
    const { unmount } = render(<AuthGuard>fleet page</AuthGuard>);
    await screen.findByText("fleet page");
    expect(screen.queryByRole("link", { name: "Administration" })).not.toBeInTheDocument();
    unmount();
    const admin = { ...alice, roles: [{ role: "administrator", customer_id: "c1", site_id: null }] };
    stubFetch({ "GET /api/auth/me": { status: 200, body: admin } });
    render(<AuthGuard>fleet page</AuthGuard>);
    expect(await screen.findByRole("link", { name: "Administration" })).toHaveAttribute("href", expect.stringMatching(/^\/admin\/relays\/?$/));
  });

  it("shows an error when the server fails", async () => {
    stubFetch({ "GET /api/auth/me": { status: 500, body: { error: "internal error" } } });
    render(<AuthGuard>fleet page</AuthGuard>);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
    expect(replace).not.toHaveBeenCalled();
  });

  it("signs out with the CSRF header", async () => {
    const fetchMock = stubFetch({
      "GET /api/auth/me": { status: 200, body: alice },
      "POST /api/auth/logout": { status: 204 },
    });
    render(<AuthGuard>fleet page</AuthGuard>);
    await userEvent.click(await screen.findByRole("button", { name: "Sign out" }));
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith(LOGIN_PATH));
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(fetchMock.mock.calls[1]?.[0]).toBe("/api/auth/logout");
    expect(requestHeaders(fetchMock, 1)[CSRF_HEADER]).toBe("1");
  });
});
