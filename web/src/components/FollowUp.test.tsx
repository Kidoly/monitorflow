import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { FollowUpPolicyList } from "@/lib/api";
import { As, customerAdmin } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { FollowUpSection } from "./FollowUp";

vi.mock("next/navigation", () => import("@/test/navigation"));

const LIST: FollowUpPolicyList = {
  policies: [
    {
      id: "p1",
      customer_id: "c1",
      customer_name: "acme",
      site_id: null,
      site_name: null,
      steps: [
        {
          after_minutes: 10,
          on_call: [{ id: "s1", name: "Ops" }],
          users: [{ id: "u2", name: "bob" }],
          channels: [],
          administrators: false,
        },
      ],
      updated_at: "2026-11-02T10:00:00Z",
    },
  ],
  default_steps: [{ after_minutes: 15, on_call: [], users: [], channels: [], administrators: true }],
  schedules: [
    { id: "s1", name: "Ops", customer_id: "c1", site_id: null },
    { id: "s2", name: "Lyon", customer_id: null, site_id: "site-2" },
  ],
  channels: [{ id: "n1", name: "Slack #managers", customer_id: "c1", site_id: null }],
};

const places = {
  "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 2 }] } },
  "GET /api/sites": {
    status: 200,
    body: {
      sites: [
        { id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme", server_count: 1, relay_count: 0 },
        { id: "site-2", name: "lyon", customer_id: "c1", customer_name: "acme", server_count: 1, relay_count: 0 },
      ],
    },
  },
  "GET /api/on-call/people?customer_id=c1": {
    status: 200,
    body: { people: [{ id: "u2", username: "bob", has_contact: true }, { id: "u3", username: "carol", has_contact: false }] },
  },
};

const show = () =>
  render(
    <As user={customerAdmin}>
      <FollowUpSection enabled />
    </As>,
  );

describe("FollowUpSection", () => {
  it("states the default and each place's steps in sentences", async () => {
    stubFetch({ "GET /api/follow-up/policies": { status: 200, body: LIST } });
    show();
    expect(await screen.findByText(/Critical alerts not acknowledged within 15 minutes go to the nearest administrators/)).toBeInTheDocument();
    const acme = screen.getByRole("group", { name: "Follow-up for All of acme" });
    expect(within(acme).getByText("After 10 minutes without acknowledgement, tell the person on call of Ops and bob.")).toBeInTheDocument();
  });

  it("edits the steps of a place, offering only what covers all of it", async () => {
    const fetchMock = stubFetch({
      "GET /api/follow-up/policies": { status: 200, body: LIST },
      ...places,
      "PUT /api/follow-up/policies": { status: 200, body: LIST.policies[0] },
    });
    const user = userEvent.setup();
    show();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const step = await screen.findByRole("listitem", { name: "Step 1" });
    // Lyon's schedule does not cover all of acme.
    expect(within(step).queryByRole("checkbox", { name: "Lyon" })).not.toBeInTheDocument();
    expect(within(step).getByRole("checkbox", { name: "Ops" })).toBeChecked();
    await within(step).findByRole("checkbox", { name: /carol/ });
    await user.click(within(step).getByRole("checkbox", { name: "Slack #managers" }));
    await user.click(screen.getByRole("button", { name: "Add a step" }));
    const second = screen.getByRole("listitem", { name: "Step 2" });
    await user.click(within(second).getByRole("checkbox", { name: "The administrators of the place" }));
    await user.click(screen.getByRole("button", { name: "Move step 2 up" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toEqual({
      customer_id: "c1",
      site_id: null,
      steps: [
        { after_minutes: 15, on_call: [], users: [], channels: [], administrators: true },
        { after_minutes: 10, on_call: ["s1"], users: ["u2"], channels: ["n1"], administrators: false },
      ],
    });
  });

  it("shows the server's reason for refusing steps", async () => {
    stubFetch({
      "GET /api/follow-up/policies": { status: 200, body: LIST },
      ...places,
      "PUT /api/follow-up/policies": { status: 400, body: { error: "Step 1 tells nobody: choose who it tells, or remove it." } },
    });
    const user = userEvent.setup();
    show();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("Step 1 tells nobody");
  });

  it("goes back to the default", async () => {
    const fetchMock = stubFetch({
      "GET /api/follow-up/policies": [{ status: 200, body: LIST }, { status: 200, body: { ...LIST, policies: [] } }],
      "DELETE /api/follow-up/policies/p1": { status: 204 },
    });
    const user = userEvent.setup();
    show();
    await user.click(await screen.findByRole("button", { name: "Back to the default…" }));
    await user.click(screen.getByRole("button", { name: "Use the default" }));
    await vi.waitFor(() => expect(screen.queryByRole("group", { name: /Follow-up for/ })).not.toBeInTheDocument());
    expect(fetchMock.mock.calls.some(([url, init]) => url === "/api/follow-up/policies/p1" && init?.method === "DELETE")).toBe(true);
  });

  it("stays out of the way of those who do not administer anything", async () => {
    stubFetch({ "GET /api/follow-up/policies": { status: 403, body: { error: "forbidden" } } });
    const { container } = show();
    await vi.waitFor(() => expect(container).toBeEmptyDOMElement());
  });
});
