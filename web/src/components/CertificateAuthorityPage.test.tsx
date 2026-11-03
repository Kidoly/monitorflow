import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { As, customerAdmin, me } from "@/test/admin";
import { authority, peer, rotation } from "@/test/certificateAuthority";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";

import { CertificateAuthorityPage } from "./CertificateAuthorityPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const URL = "/api/certificate-authority";

function renderPage(user = globalAdmin) {
  return render(
    <As user={user}>
      <CertificateAuthorityPage />
    </As>,
  );
}

const posted = (fetchMock: ReturnType<typeof stubFetch>, url: string) =>
  fetchMock.mock.calls.filter(([u, init]) => u === url && init?.method === "POST").map(([, init]) => init?.body);

describe("CertificateAuthorityPage", () => {
  beforeEach(() => setUrl("/admin/certificate-authority/"));

  it("is for global administrators only", () => {
    stubFetch({});
    renderPage(customerAdmin);
    expect(screen.getByText("The certificate authority needs a global administrator")).toBeInTheDocument();
  });

  it("shows the dates, the fingerprint under Details, and a planned change", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [`GET ${URL}`]: [
        { status: 200, body: authority() },
        { status: 200, body: authority({ rotation: rotation() }) },
      ],
      [`POST ${URL}/rotation`]: { status: 202, body: authority({ rotation: rotation() }) },
    });
    renderPage();
    expect(await screen.findByText("Valid until 2036-09-29")).toBeInTheDocument();
    expect(screen.getByText("2026-10-01")).toBeInTheDocument();
    expect(screen.getByText("Details")).toBeInTheDocument();
    expect(screen.getByText("a".repeat(64))).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Change the certificate authority…" }));
    await user.click(screen.getByRole("button", { name: "Start the change" }));
    expect(posted(fetchMock, `${URL}/rotation`)).toEqual([JSON.stringify({ emergency: false })]);
    expect(await screen.findByText("Change under way: step 1 of 2")).toBeInTheDocument();
  });

  it("asks for an explicit acknowledgement before an emergency change", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [`GET ${URL}`]: { status: 200, body: authority() },
      [`POST ${URL}/rotation`]: { status: 409, body: { error: "a change of certificate authority is already under way" } },
    });
    renderPage();
    await user.click(await screen.findByRole("button", { name: "Emergency change…" }));
    expect(screen.getByRole("form", { name: "Start an emergency change?" })).toHaveTextContent(/may have leaked/);
    const confirm = screen.getByRole("button", { name: "Start the emergency change" });
    expect(confirm).toBeDisabled();
    await user.click(screen.getByRole("checkbox", { name: /must be installed again/ }));
    await user.click(confirm);
    expect(posted(fetchMock, `${URL}/rotation`)).toEqual([JSON.stringify({ emergency: true })]);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Cannot start the change: a change of certificate authority is already under way.",
    );
  });

  it("warns when the authority expires soon", async () => {
    stubFetch({
      [`GET ${URL}`]: {
        status: 200,
        body: authority({ expires_soon: true, expires_at: "2027-06-01T12:00:00Z", auto_change_at: "2099-12-03T12:00:00Z" }),
      },
    });
    renderPage();
    expect(await screen.findByText(/The certificate authority expires on 2027-06-01\. Change it now/)).toBeInTheDocument();
    expect(screen.getByText(/the change starts by itself on 2099-12-03/)).toBeInTheDocument();
  });

  it("follows a change: progress, who is waited for, and the next step", async () => {
    const user = userEvent.setup();
    const fetchMock = stubFetch({
      [`GET ${URL}`]: { status: 200, body: authority({ rotation: rotation() }) },
      [`POST ${URL}/rotation/next`]: { status: 200, body: authority({ rotation: rotation({ stage: "switched" }) }) },
    });
    renderPage();
    expect(await screen.findByText(/312 of 315 servers and relays learned the new certificate authority/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Change the certificate authority…" })).not.toBeInTheDocument();
    const table = screen.getByRole("table", { name: "Waited for" });
    expect(within(table).getByText("web-01.example")).toBeInTheDocument();
    expect(within(table).getByText("Relay")).toBeInTheDocument();
    expect(within(table).getAllByText("Server").length).toBe(2);
    expect(within(table).getByText("Too old to follow: updates itself")).toBeInTheDocument();
    expect(within(table).getByText("never")).toBeInTheDocument();
    expect(within(table).getAllByText("2026-11-09").length).toBe(3);

    await user.click(screen.getByRole("button", { name: "Switch now…" }));
    expect(screen.getByRole("form", { name: "Switch to the new certificate authority now?" })).toHaveTextContent(
      /The 3 servers and relays not done yet will not be able to connect/,
    );
    await user.click(screen.getByRole("button", { name: "Switch now" }));
    expect(posted(fetchMock, `${URL}/rotation/next`)).toEqual([JSON.stringify({})]);
  });

  it("shows an emergency, a restored retired authority and machines that need a new install", async () => {
    stubFetch({
      [`GET ${URL}`]: {
        status: 200,
        body: authority({
          rotation: rotation({ stage: "switched", emergency: true, switched_at: "2026-11-03T10:00:00Z", distrust_at: "2026-11-04T10:00:00Z" }),
          restored_retired_at: "2026-10-20T12:00:00Z",
          cut_off: [peer({ peer_id: "old-1", name: "old-1.example", cut_off: true })],
        }),
      },
    });
    renderPage();
    expect(await screen.findByText("Emergency change under way")).toBeInTheDocument();
    expect(screen.getByText("Old one stops being trusted")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Stop trusting the old authority now…" })).toBeInTheDocument();
    expect(screen.getByText(/A restore brought back a certificate authority this server retired on 2026-10-20/)).toBeInTheDocument();
    const cutOff = screen.getByRole("table", { name: "Needs a new install" });
    expect(within(cutOff).getByText("old-1.example")).toBeInTheDocument();
    expect(screen.getByText(/single-use token for its name/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Add servers" })).toHaveAttribute("href", "/admin/tokens");
  });
});
