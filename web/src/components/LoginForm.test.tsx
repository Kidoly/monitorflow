import { act, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { CSRF_HEADER } from "@/lib/api";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { LoginForm, RETRY_SECONDS } from "./LoginForm";

const replace = vi.fn();
// Like Next's, the router object is stable across renders.
const router = { replace };
vi.mock("next/navigation", () => ({ useRouter: () => router }));

const HEALTHY = { "GET /api/health": { status: 200, body: { status: "ok" } } };

async function signIn(password = "correct horse", user = userEvent.setup()) {
  render(<LoginForm />);
  await user.type(screen.getByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), password);
  await user.keyboard("{Enter}");
}

describe("LoginForm", () => {
  beforeEach(() => replace.mockReset());
  afterEach(() => vi.useRealTimers());

  it("posts the credentials with the CSRF header and opens the fleet", async () => {
    const fetchMock = stubFetch({
      ...HEALTHY,
      "POST /api/auth/login": { status: 200, body: { id: "u1", username: "alice", roles: [] } },
    });
    await signIn();
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith("/"));
    const login = fetchMock.mock.calls.findIndex(([url]) => url === "/api/auth/login");
    expect(requestHeaders(fetchMock, login)[CSRF_HEADER]).toBe("1");
    expect(fetchMock.mock.calls[login]?.[1]?.credentials).toBe("same-origin");
    expect(JSON.parse(fetchMock.mock.calls[login]?.[1]?.body as string)).toEqual({
      username: "alice",
      password: "correct horse",
    });
  });

  it("says the credentials are wrong, marks both fields and clears the password", async () => {
    stubFetch({ ...HEALTHY, "POST /api/auth/login": { status: 401, body: { error: "invalid credentials" } } });
    await signIn("wrong");
    expect(await screen.findByRole("alert")).toHaveTextContent("Wrong username or password. Check both and try again.");
    expect(screen.getByLabelText("Password")).toHaveValue("");
    expect(screen.getByLabelText("Username")).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByLabelText("Password")).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled();
    expect(replace).not.toHaveBeenCalled();
  });

  it("does not send the credentials to an unreachable server and retries", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const fetchMock = stubFetch({
      "GET /api/health": [new TypeError("Failed to fetch"), new TypeError("Failed to fetch"), { status: 200, body: { status: "ok" } }],
    });
    await signIn("correct horse", userEvent.setup({ advanceTimers: vi.advanceTimersByTime }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Server unreachable. localhost:3000 did not respond. Your credentials were not sent.",
    );
    expect(fetchMock.mock.calls.some(([url]) => url === "/api/auth/login")).toBe(false);
    expect(screen.getByRole("button", { name: `Retrying in ${RETRY_SECONDS} s…` })).toBeDisabled();

    await act(() => vi.advanceTimersByTimeAsync(1000));
    expect(screen.getByRole("button", { name: "Retrying in 4 s…" })).toBeDisabled();
    // The first check fails, the second one succeeds and sign-in is possible again.
    await act(() => vi.advanceTimersByTimeAsync(RETRY_SECONDS * 2000));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled();
  });

  it("does not claim the credentials stayed when the login request itself fails", async () => {
    stubFetch({ ...HEALTHY, "POST /api/auth/login": new TypeError("Failed to fetch") });
    await signIn();
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Server unreachable.");
    expect(alert).not.toHaveTextContent("not sent");
  });
});
