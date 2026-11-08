import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { LicenseView } from "@/lib/api";
import { As, customerAdmin, me, single } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { LicensePage } from "./LicensePage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });

const valid: LicenseView = {
  available: true,
  state: "valid",
  license: {
    id: "6f1c1d1e-8a43-4c55-9d43-3c2f6c0e9a01",
    customer: "Example Corp",
    edition: "enterprise",
    server_instances: 3,
    issued: "2026-11-01",
    expires: "2027-11-30",
  },
  problem: null,
  installed_at: "2026-11-07T10:00:00Z",
  installed_by: "alice",
};

const none: LicenseView = { available: true, state: "none", license: null, problem: null, installed_at: null, installed_by: null };

function renderPage(user = globalAdmin) {
  return render(
    <As user={user}>
      <LicensePage />
    </As>,
  );
}

describe("LicensePage", () => {
  beforeEach(() => setUrl("/admin/license/"));

  it("is for global administrators only", () => {
    stubFetch({});
    renderPage(customerAdmin);
    expect(screen.getByText("The license needs a global administrator")).toBeInTheDocument();
  });

  it("says a single central server needs no license key", async () => {
    stubFetch({ "GET /api/license": { status: 200, body: none } });
    renderPage();
    expect(await screen.findByText("No license key.")).toBeInTheDocument();
    expect(screen.getByText(/A single central server needs none/)).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Install a license key" })).toBeInTheDocument();
  });

  it("installs a pasted license key", async () => {
    const user = userEvent.setup();
    const fetch = stubFetch({
      "GET /api/license": [
        { status: 200, body: none },
        { status: 200, body: valid },
      ],
      "PUT /api/license": { status: 200, body: valid },
    });
    renderPage();
    await user.type(await screen.findByRole("textbox", { name: /License key/ }), "-----BEGIN MONITORFLOW LICENSE-----");
    await user.click(screen.getByRole("button", { name: "Install" }));
    expect(await screen.findByText("Valid until 30 November 2027, for 3 central servers.")).toBeInTheDocument();
    const put = fetch.mock.calls.find(([, init]) => (init as RequestInit | undefined)?.method === "PUT");
    expect(JSON.parse(String((put?.[1] as RequestInit).body))).toEqual({ text: "-----BEGIN MONITORFLOW LICENSE-----" });
    expect(screen.getByText("Example Corp")).toBeInTheDocument();
    expect(screen.getByText("3 central servers, the first one included")).toBeInTheDocument();
  });

  it("shows why a license key is refused", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/license": { status: 200, body: none },
      "PUT /api/license": { status: 400, body: { error: "This license expired on 2020-01-01" } },
    });
    renderPage();
    await user.type(await screen.findByRole("textbox", { name: /License key/ }), "old");
    await user.click(screen.getByRole("button", { name: "Install" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("This license expired on 2020-01-01.");
  });

  it("removes the license key after saying what keeps running", async () => {
    const user = userEvent.setup();
    stubFetch({
      "GET /api/license": [
        { status: 200, body: valid },
        { status: 200, body: none },
      ],
      "DELETE /api/license": { status: 204 },
    });
    renderPage();
    await user.click(await screen.findByRole("button", { name: "Remove the license key…" }));
    expect(screen.getByText(/Central servers already added keep running and monitoring goes on/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove the license key" }));
    expect(await screen.findByText("No license key.")).toBeInTheDocument();
  });

  it("never says customer, whoever reads it (ADR-0104)", async () => {
    for (const user of [globalAdmin, single(globalAdmin)]) {
      stubFetch({ "GET /api/license": { status: 200, body: valid } });
      const { unmount } = renderPage(user);
      // The license holder's name is a fact labelled "For", not a customer.
      expect(await screen.findByText("Example Corp")).toBeInTheDocument();
      expect(screen.getByText("For")).toBeInTheDocument();
      expect(document.body.textContent).not.toMatch(/customer/i);
      unmount();
    }
  });

  it("says when the build has no Enterprise edition", async () => {
    stubFetch({ "GET /api/license": { status: 200, body: { available: false } } });
    renderPage();
    expect(await screen.findByText(/does not include the Enterprise edition/)).toBeInTheDocument();
    expect(screen.queryByRole("textbox")).not.toBeInTheDocument();
  });
});
