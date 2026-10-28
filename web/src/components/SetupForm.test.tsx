import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { LOGIN_PATH } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { SetupForm } from "./SetupForm";

const replace = vi.fn();
const router = { replace };
vi.mock("next/navigation", () => ({ useRouter: () => router }));

const TOKEN = "ab".repeat(32);
const PENDING = { "GET /api/setup": { status: 200, body: { pending: true } } };
const PASSWORD = "correct horse battery";

async function fill(password = PASSWORD, confirm = password) {
  const user = userEvent.setup();
  render(<SetupForm />);
  const username = await screen.findByLabelText("Username");
  expect(username).toHaveValue("admin");
  await user.type(screen.getByLabelText("Password"), password);
  await user.type(screen.getByLabelText("Confirm password"), confirm);
  await user.click(screen.getByRole("button", { name: "Create and sign in" }));
}

describe("SetupForm", () => {
  beforeEach(() => {
    replace.mockReset();
    window.history.replaceState(null, "", `/setup/#${TOKEN}`);
  });
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("creates the administrator with the link's token and opens the fleet", async () => {
    const fetchMock = stubFetch({
      ...PENDING,
      "POST /api/setup": { status: 200, body: { id: "u1", username: "admin", roles: [] } },
    });
    await fill();
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith("/"));
    // The secret leaves the address bar as soon as it is read.
    expect(window.location.hash).toBe("");
    const call = fetchMock.mock.calls.find(([url, init]) => url === "/api/setup" && init?.method === "POST");
    expect(JSON.parse(call?.[1]?.body as string)).toEqual({ token: TOKEN, username: "admin", password: PASSWORD });
  });

  it("checks that both passwords match before sending", async () => {
    const fetchMock = stubFetch(PENDING);
    await fill(PASSWORD, "something else entirely");
    expect(await screen.findByRole("alert")).toHaveTextContent("The passwords differ.");
    expect(fetchMock.mock.calls.some(([, init]) => init?.method === "POST")).toBe(false);
  });

  it("explains how to get a new link when this one is no longer valid", async () => {
    stubFetch({ ...PENDING, "POST /api/setup": { status: 410, body: { error: "gone" } } });
    await fill();
    expect(await screen.findByText("This setup link is no longer valid.")).toBeInTheDocument();
    expect(screen.getByText(/monitorflow-server setup-link/)).toBeInTheDocument();
  });

  it("shows the server's reason for a refused username or password", async () => {
    stubFetch({
      ...PENDING,
      "POST /api/setup": { status: 400, body: { error: "password must be at least 12 characters" } },
    });
    await fill();
    expect(await screen.findByRole("alert")).toHaveTextContent("Password must be at least 12 characters.");
  });

  it("says the link is missing when the address has none", async () => {
    window.history.replaceState(null, "", "/setup/");
    stubFetch(PENDING);
    render(<SetupForm />);
    expect(await screen.findByText("This address has no setup link.")).toBeInTheDocument();
  });

  it("sends the user to sign in once setup is done", async () => {
    stubFetch({ "GET /api/setup": { status: 200, body: { pending: false } } });
    render(<SetupForm />);
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith(LOGIN_PATH));
    expect(screen.queryByLabelText("Password")).not.toBeInTheDocument();
  });
});
