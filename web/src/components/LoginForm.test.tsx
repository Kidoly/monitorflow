import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { CSRF_HEADER } from "@/lib/api";
import { requestHeaders, stubFetch } from "@/test/fetch";

import { LoginForm } from "./LoginForm";

const replace = vi.fn();
// Like Next's, the router object is stable across renders.
const router = { replace };
vi.mock("next/navigation", () => ({ useRouter: () => router }));

async function signIn(password = "correct horse") {
  const user = userEvent.setup();
  render(<LoginForm />);
  await user.type(screen.getByLabelText("Username"), "alice");
  await user.type(screen.getByLabelText("Password"), password);
  await user.keyboard("{Enter}");
}

describe("LoginForm", () => {
  beforeEach(() => replace.mockReset());

  it("posts the credentials with the CSRF header and opens the fleet", async () => {
    const fetchMock = stubFetch({
      "POST /api/auth/login": { status: 200, body: { id: "u1", username: "alice", roles: [] } },
    });
    await signIn();
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith("/"));
    expect(requestHeaders(fetchMock)[CSRF_HEADER]).toBe("1");
    expect(fetchMock.mock.calls[0]?.[1]?.credentials).toBe("same-origin");
    expect(JSON.parse(fetchMock.mock.calls[0]?.[1]?.body as string)).toEqual({
      username: "alice",
      password: "correct horse",
    });
  });

  it("shows an error and clears the password on bad credentials", async () => {
    stubFetch({ "POST /api/auth/login": { status: 401, body: { error: "invalid credentials" } } });
    await signIn("wrong");
    expect(await screen.findByRole("alert")).toHaveTextContent("Invalid username or password.");
    expect(screen.getByLabelText("Password")).toHaveValue("");
    expect(screen.getByRole("button", { name: "Sign in" })).toBeEnabled();
    expect(replace).not.toHaveBeenCalled();
  });

  it("says when the server cannot be reached", async () => {
    stubFetch({ "POST /api/auth/login": new TypeError("Failed to fetch") });
    await signIn();
    expect(await screen.findByRole("alert")).toHaveTextContent("Cannot reach the MonitorFlow server.");
  });
});
