import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { DashboardSummary, DisplayLink } from "@/lib/api";
import { stubFetch } from "@/test/fetch";

import { DisplayLinks, linkSummary } from "./DisplayLinks";

const LINK: DisplayLink = {
  id: "l1",
  name: "Reception TV",
  dashboards: [{ id: "d1", name: "NOC" }],
  other_dashboards: 0,
  seconds_per_page: 30,
  created_by_username: "alice",
  created_at: "2026-11-01T10:00:00Z",
  expires_at: null,
  last_used_at: null,
};

function summary(id: string, name: string, canLink: boolean): DashboardSummary {
  return {
    id,
    name,
    shared_with: { kind: "personal" },
    panels: 1,
    updated_at: "2026-11-01T10:00:00Z",
    updated_by_username: null,
    can_edit: true,
    can_link: canLink,
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("linkSummary", () => {
  it("says what a link shows, in turn", () => {
    expect(linkSummary(LINK)).toBe("Shows NOC");
    expect(linkSummary({ ...LINK, dashboards: [...LINK.dashboards, { id: "d2", name: "Disks" }], other_dashboards: 1 })).toBe(
      "Shows NOC, then Disks, then 1 more, 30 s each",
    );
  });
});

describe("DisplayLinks", () => {
  it("lists links with their last use, and turns one off", async () => {
    const fetchMock = stubFetch({
      "GET /api/dashboards/d1/display-links": [
        { status: 200, body: { links: [{ ...LINK, expires_at: "2027-11-01T10:00:00Z" }] } },
        { status: 200, body: { links: [] } },
      ],
      "DELETE /api/display-links/l1": { status: 204 },
    });
    render(<DisplayLinks dashboard={{ id: "d1", name: "NOC" }} />);
    expect(await screen.findByText("Reception TV")).toBeInTheDocument();
    expect(screen.getByText(/Made by alice\. Expires .*\. Not used yet\./)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Turn off Reception TV" }));
    await userEvent.click(screen.getByRole("button", { name: "Turn off" }));
    expect(fetchMock.mock.calls.some((c) => String(c[0]) === "/api/display-links/l1" && c[1]?.method === "DELETE")).toBe(true);
    expect(await screen.findByRole("button", { name: "New display link" })).toBeInTheDocument();
  });

  it("makes a link that cycles, and shows its address once", async () => {
    const fetchMock = stubFetch({
      "GET /api/dashboards/d1/display-links": { status: 200, body: { links: [] } },
      "GET /api/dashboards": {
        status: 200,
        body: { dashboards: [summary("d1", "NOC", true), summary("d2", "Disks", true), summary("d3", "Not mine", false)] },
      },
      "POST /api/display-links": { status: 201, body: { ...LINK, url: "https://mf.example/display/#abc" } },
    });
    render(<DisplayLinks dashboard={{ id: "d1", name: "NOC" }} />);
    await userEvent.click(await screen.findByRole("button", { name: "New display link" }));
    const form = screen.getByRole("form", { name: "New display link" });
    await userEvent.type(within(form).getByLabelText("Where is the screen?"), "Reception TV");
    expect(within(form).queryByLabelText("Not mine")).not.toBeInTheDocument();
    await userEvent.click(await within(form).findByLabelText("Disks"));
    await userEvent.selectOptions(within(form).getByLabelText("Each dashboard stays"), "60");
    await userEvent.selectOptions(within(form).getByLabelText(/The link stops working/), "30d");
    await userEvent.click(within(form).getByRole("button", { name: "Make the link" }));
    const call = fetchMock.mock.calls.find((c) => String(c[0]) === "/api/display-links");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({
      name: "Reception TV",
      dashboard_ids: ["d1", "d2"],
      seconds_per_page: 60,
      expires: "30d",
    });
    expect(await screen.findByDisplayValue("https://mf.example/display/#abc")).toBeInTheDocument();
    expect(screen.getByText(/It is shown only now/)).toBeInTheDocument();
  });
});
