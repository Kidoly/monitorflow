import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";

import { WelcomeForm } from "./WelcomeForm";

const replace = vi.fn();
const router = { replace };
vi.mock("next/navigation", () => ({ useRouter: () => router }));

const TOKEN = "cd".repeat(32);
const PASSWORD = "correct horse battery";
const INVITED = { "POST /api/password-link": { status: 200, body: { username: "bob", invited: true } } };

async function fill(password = PASSWORD, confirm = password) {
  const user = userEvent.setup();
  render(<WelcomeForm />);
  expect(await screen.findByText("bob")).toBeInTheDocument();
  await user.type(screen.getByLabelText("Password"), password);
  await user.type(screen.getByLabelText("Confirm password"), confirm);
  await user.click(screen.getByRole("button", { name: "Save and sign in" }));
}

describe("WelcomeForm", () => {
  beforeEach(() => {
    replace.mockReset();
    window.history.replaceState(null, "", `/welcome/#${TOKEN}`);
  });
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("greets the invited user, sets the password and opens the fleet", async () => {
    const fetchMock = stubFetch({
      ...INVITED,
      "POST /api/password-link/use": { status: 200, body: { id: "u2", username: "bob", roles: [] } },
    });
    await fill();
    await vi.waitFor(() => expect(replace).toHaveBeenCalledWith("/"));
    expect(screen.getByRole("heading", { name: "Welcome to MonitorFlow" })).toBeInTheDocument();
    expect(window.location.hash).toBe("");
    const call = fetchMock.mock.calls.find(([url]) => url === "/api/password-link/use");
    expect(JSON.parse(call?.[1]?.body as string)).toEqual({ token: TOKEN, password: PASSWORD });
  });

  it("checks that both passwords match before sending", async () => {
    const fetchMock = stubFetch(INVITED);
    await fill(PASSWORD, "something else entirely");
    expect(screen.getByText("The passwords differ.")).toBeInTheDocument();
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("says when the link is dead", async () => {
    stubFetch({ "POST /api/password-link": { status: 410, body: { error: "gone" } } });
    render(<WelcomeForm />);
    expect(await screen.findByText("This link cannot be used.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "sign in" })).toHaveAttribute("href", expect.stringMatching(/^\/login/));
  });

  it("says when there is no link at all", async () => {
    window.history.replaceState(null, "", "/welcome/");
    const fetchMock = stubFetch({});
    render(<WelcomeForm />);
    expect(await screen.findByText("This link cannot be used.")).toBeInTheDocument();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("shows the server's reason for a refused password", async () => {
    stubFetch({
      ...INVITED,
      "POST /api/password-link/use": { status: 400, body: { error: "password must be at least 12 characters" } },
    });
    await fill();
    expect(await screen.findByText("Password must be at least 12 characters.")).toBeInTheDocument();
  });
});
