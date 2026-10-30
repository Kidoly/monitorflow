import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { CSRF_HEADER, LOGIN_PATH } from "@/lib/api";
import { requestHeaders, stubFetch } from "@/test/fetch";
import { router, setUrl } from "@/test/navigation";
import { SHELL_ROUTES } from "@/test/shell";

import { AuthGuard } from "./AuthGuard";

vi.mock("next/navigation", () => import("@/test/navigation"));

const alice = { id: "u1", username: "alice", roles: [{ role: "viewer", site_id: null }] };

describe("AuthGuard", () => {
  beforeEach(() => {
    router.replace.mockClear();
    setUrl("/");
  });

  it("redirects to the login page on 401 without rendering the page", async () => {
    stubFetch({ "GET /api/auth/me": { status: 401, body: { error: "authentication required" } } });
    render(<AuthGuard>secret page</AuthGuard>);
    await vi.waitFor(() => expect(router.replace).toHaveBeenCalledWith(LOGIN_PATH));
    expect(screen.queryByText("secret page")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("shows the page in the shell with the current user", async () => {
    stubFetch({ ...SHELL_ROUTES, "GET /api/auth/me": { status: 200, body: alice } });
    render(<AuthGuard>fleet page</AuthGuard>);
    await screen.findByText("fleet page");
    expect(screen.getByRole("main")).toHaveTextContent("fleet page");
    expect(screen.getByRole("navigation", { name: "Main" })).toHaveTextContent("alice · viewer");
  });

  it("shows Administration to administrators only", async () => {
    stubFetch({ ...SHELL_ROUTES, "GET /api/auth/me": { status: 200, body: alice } });
    const { unmount } = render(<AuthGuard>fleet page</AuthGuard>);
    await screen.findByText("fleet page");
    expect(screen.queryByText("Administration")).not.toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Relays" })).not.toBeInTheDocument();
    unmount();
    const admin = { ...alice, roles: [{ role: "administrator", customer_id: "c1", site_id: null }] };
    stubFetch({ ...SHELL_ROUTES, "GET /api/auth/me": { status: 200, body: admin } });
    render(<AuthGuard>fleet page</AuthGuard>);
    expect(await screen.findByRole("link", { name: "Relays" })).toHaveAttribute("href", expect.stringMatching(/^\/admin\/relays\/?$/));
    expect(screen.getByRole("link", { name: "Sites" })).toHaveAttribute("href", expect.stringMatching(/^\/admin\/sites\/?$/));
    expect(screen.getByRole("link", { name: "Add servers" })).toHaveAttribute("href", expect.stringMatching(/^\/admin\/tokens\/?$/));
  });

  it("shows Campaigns to customer-wide roles only", async () => {
    const site = { ...alice, roles: [{ role: "administrator", customer_id: null, site_id: "s1" }] };
    stubFetch({ ...SHELL_ROUTES, "GET /api/auth/me": { status: 200, body: site } });
    const { unmount } = render(<AuthGuard>fleet page</AuthGuard>);
    await screen.findByText("fleet page");
    expect(screen.queryByRole("link", { name: "Campaigns" })).not.toBeInTheDocument();
    unmount();
    stubFetch({ ...SHELL_ROUTES, "GET /api/auth/me": { status: 200, body: alice } });
    render(<AuthGuard>fleet page</AuthGuard>);
    expect(await screen.findByRole("link", { name: "Campaigns" })).toHaveAttribute("href", expect.stringMatching(/^\/campaigns\/?$/));
  });

  it("shows an error when the server fails", async () => {
    stubFetch({ "GET /api/auth/me": { status: 500, body: { error: "internal error" } } });
    render(<AuthGuard>fleet page</AuthGuard>);
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
    expect(router.replace).not.toHaveBeenCalled();
  });

  it("signs out with the CSRF header", async () => {
    const fetchMock = stubFetch({
      ...SHELL_ROUTES,
      "GET /api/auth/me": { status: 200, body: alice },
      "POST /api/auth/logout": { status: 204 },
    });
    render(<AuthGuard>fleet page</AuthGuard>);
    await userEvent.click(await screen.findByRole("button", { name: "Sign out" }));
    await vi.waitFor(() => expect(router.replace).toHaveBeenCalledWith(LOGIN_PATH));
    const logout = fetchMock.mock.calls.findIndex(([url]) => url === "/api/auth/logout");
    expect(logout).toBeGreaterThan(-1);
    expect(requestHeaders(fetchMock, logout)[CSRF_HEADER]).toBe("1");
  });
});
