import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { REFRESH_MS } from "@/lib/usePolledData";
import { As, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";
import { setUrl } from "@/test/navigation";
import { CUSTOMER_ID, INCIDENT_ID, PENDING_ID, pending } from "@/test/remediation";

import { ApprovalQueue } from "./ApprovalQueue";

vi.mock("next/navigation", () => import("@/test/navigation"));

// 20 min 5 s before the fixture's deadline.
const NOW = new Date("2026-10-19T11:39:55Z");
const LIST = "GET /api/remediation/approvals";
const APPROVE = `POST /api/remediation/approvals/${PENDING_ID}/approve`;
const REJECT = `POST /api/remediation/approvals/${PENDING_ID}/reject`;
const list = (...approvals: ReturnType<typeof pending>[]) => ({ status: 200, body: { approvals } });
const decision = (state: "approved" | "rejected", command: string, edited = false) => ({
  status: 200,
  body: { id: PENDING_ID, incident_id: INCIDENT_ID, state, command, edited },
});

const table = () => screen.findByRole("table", { name: "Commands waiting for approval" });
const review = async () => userEvent.click(await screen.findByRole("button", { name: "Review the command for web-1" }));
const bodyOf = (fetchMock: ReturnType<typeof stubFetch>, key: string) => {
  const call = fetchMock.mock.calls.find(([url, init]) => `${init?.method ?? "GET"} ${String(url)}` === key);
  return call && JSON.parse(String(call[1]?.body));
};

describe("ApprovalQueue", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl("/remediation/approvals/");
  });
  afterEach(() => vi.useRealTimers());

  it("lists waiting commands with their tier and a deadline counting down", async () => {
    stubFetch({ [LIST]: list(pending(), pending({ id: "p2", hostname: "db-1", tier: "high_impact", command: "apt-get autoremove -y" })) });
    render(<ApprovalQueue />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(
      /^Disk used \/var on web-1Step 3 of 10 · paris · Acme Hosting\s*Low-impact\s*journalctl --vacuum-size=500M\s*20 min 5 s left\s*Review$/,
    );
    expect(rows[2]).toHaveTextContent(/High-impact/);
    expect(screen.getByText(/2 waiting · Updated/)).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(2000));
    expect(rows[1]).toHaveTextContent("20 min 3 s left");
  });

  it("says when AI is off and how to turn it on", async () => {
    const ai = (enabled: boolean) => ({ status: 200, body: { enabled, from_config: false, has_key: false, model: "m", models: [] } });
    stubFetch({ [LIST]: list(), "GET /api/settings/ai": ai(false) });
    const { unmount } = render(
      <As user={me({ role: "administrator", customer_id: null, site_id: null })}>
        <ApprovalQueue />
      </As>,
    );
    expect(await screen.findByText("AI is off.")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Turn on AI" })).toHaveAttribute("href", "/admin/ai");
    unmount();

    render(
      <As user={me()}>
        <ApprovalQueue />
      </As>,
    );
    expect(await screen.findByText(/A global administrator can turn it on/)).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: "Turn on AI" })).not.toBeInTheDocument();
  });

  it("says nothing about AI while it is on", async () => {
    stubFetch({
      [LIST]: list(),
      "GET /api/settings/ai": { status: 200, body: { enabled: true, from_config: false, has_key: true, model: "m", models: [] } },
    });
    render(<ApprovalQueue />);
    expect(await screen.findByText("Nothing waiting for approval")).toBeInTheDocument();
    expect(screen.queryByText("AI is off.")).not.toBeInTheDocument();
  });

  it("shows the AI's text, the rule and that rollback is limited", async () => {
    stubFetch({ [LIST]: list(pending()) });
    render(<ApprovalQueue />);
    await review();
    expect(screen.getByRole("button", { name: "Close the command for web-1" })).toHaveAttribute("aria-expanded", "true");
    expect(screen.getByText("journald holds 29 GiB of /var")).toBeInTheDocument();
    expect(screen.getByText(/Generated text, untrusted/)).toBeInTheDocument();
    expect(screen.getByText(/low\.journal-vacuum · vacuums the systemd journal/)).toBeInTheDocument();
    expect(screen.getByText("Rollback limited").parentElement).toHaveTextContent(
      "Rollback limited No snapshot before this change: the server is not mapped to a Proxmox VM.",
    );
    expect(screen.getByRole("link", { name: "Audit log of this incident" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/remediation/audit/?\\?incident=${INCIDENT_ID}$`)),
    );
  });

  it("does not warn when the server is snapshotted first", async () => {
    stubFetch({ [LIST]: list(pending({ snapshots: { available: true, reason: null } })) });
    render(<ApprovalQueue />);
    await review();
    expect(screen.queryByText("Rollback limited")).not.toBeInTheDocument();
    expect(screen.getByText(/A Proxmox snapshot of the VM is taken/)).toBeInTheDocument();
  });

  it("approves as proposed and refreshes the queue", async () => {
    const fetchMock = stubFetch({
      [LIST]: [list(pending()), list()],
      [APPROVE]: decision("approved", "journalctl --vacuum-size=500M"),
    });
    render(<ApprovalQueue />);
    await review();
    await userEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(await screen.findByText(/Approved journalctl --vacuum-size=500M on web-1\. The incident resumes/)).toBeInTheDocument();
    expect(bodyOf(fetchMock, APPROVE)).toEqual({});
    expect(await screen.findByText("Nothing waiting for approval")).toBeInTheDocument();
  });

  it("approves an edited command", async () => {
    const fetchMock = stubFetch({
      [LIST]: [list(pending()), list()],
      [APPROVE]: decision("approved", "journalctl --vacuum-size=1G", true),
    });
    render(<ApprovalQueue />);
    await review();
    await userEvent.click(screen.getByRole("button", { name: "Edit" }));
    const box = screen.getByRole("textbox", { name: "Edited command" });
    expect(box).toHaveValue("journalctl --vacuum-size=500M");
    await userEvent.clear(box);
    expect(screen.getByRole("button", { name: "Approve edited command" })).toBeDisabled();
    await userEvent.type(box, "journalctl --vacuum-size=1G");
    await userEvent.click(screen.getByRole("button", { name: "Approve edited command" }));
    expect(await screen.findByText(/Approved the edited command journalctl --vacuum-size=1G on web-1/)).toBeInTheDocument();
    expect(bodyOf(fetchMock, APPROVE)).toEqual({ command: "journalctl --vacuum-size=1G" });
  });

  it("rejects with a reason only", async () => {
    const fetchMock = stubFetch({
      [LIST]: [list(pending()), list()],
      [REJECT]: decision("rejected", "journalctl --vacuum-size=500M"),
    });
    render(<ApprovalQueue />);
    await review();
    await userEvent.click(screen.getByRole("button", { name: "Reject…" }));
    expect(screen.getByText(/sent to the AI provider\. Do not include secrets/)).toBeInTheDocument();
    const submit = screen.getByRole("button", { name: "Reject command" });
    expect(submit).toBeDisabled();
    await userEvent.type(screen.getByRole("textbox", { name: "Reason" }), "  keep the logs for the audit  ");
    await userEvent.click(submit);
    expect(await screen.findByText(/Rejected journalctl --vacuum-size=500M on web-1\. The AI gets the reason/)).toBeInTheDocument();
    expect(bodyOf(fetchMock, REJECT)).toEqual({ reason: "keep the logs for the audit" });
  });

  it("says when someone decided first", async () => {
    stubFetch({
      [LIST]: [list(pending()), list()],
      [APPROVE]: { status: 409, body: { error: "the command is no longer pending" } },
    });
    render(<ApprovalQueue />);
    await review();
    await userEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(
      await screen.findByText("Not recorded for web-1: the command is no longer pending. The queue has been refreshed."),
    ).toBeInTheDocument();
    expect(await screen.findByText("Nothing waiting for approval")).toBeInTheDocument();
  });

  it("keeps the detail open with the error when a decision fails otherwise", async () => {
    stubFetch({ [LIST]: list(pending()), [APPROVE]: { status: 500, body: { error: "internal error" } } });
    render(<ApprovalQueue />);
    await review();
    await userEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("could not handle the request");
    expect(screen.getByRole("button", { name: "Approve" })).toBeEnabled();
  });

  it("offers only what the user's role allows", async () => {
    stubFetch({
      [LIST]: list(
        pending({ tier: "high_impact", can_approve: false }),
        pending({ id: "p2", hostname: "db-1", can_approve: false, can_reject: false }),
      ),
    });
    render(<ApprovalQueue />);
    await review();
    expect(screen.queryByRole("button", { name: "Approve" })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Edit" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Reject…" })).toBeInTheDocument();
    expect(screen.getByText("Only an administrator can approve a high-impact command.")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Review the command for db-1" }));
    expect(screen.queryByRole("button", { name: "Reject…" })).not.toBeInTheDocument();
    expect(screen.getByText("Your role can read this command but not decide on it.")).toBeInTheDocument();
  });

  it("stops offering decisions once the deadline passed", async () => {
    stubFetch({ [LIST]: list(pending({ deadline: "2026-10-19T11:39:00Z" })) });
    render(<ApprovalQueue />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows[1]).toHaveTextContent("Passed");
    await review();
    expect(screen.getByText("Passed: the incident escalates to a human.")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Approve" })).not.toBeInTheDocument();
  });

  it("filters by tier and scope", async () => {
    setUrl(`/remediation/approvals/?customer=${CUSTOMER_ID}&tier=high_impact`);
    stubFetch({
      [LIST]: list(
        pending(),
        pending({ id: "p2", hostname: "db-1", tier: "high_impact" }),
        pending({ id: "p3", hostname: "mail-1", tier: "high_impact", customer_id: "c2" }),
      ),
    });
    render(<ApprovalQueue />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows).toHaveLength(2);
    expect(rows[1]).toHaveTextContent(/on db-1/);
    expect(screen.getByText(/2 waiting/)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Audit log" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/remediation/audit/?\\?customer=${CUSTOMER_ID}$`)),
    );
  });

  it("shows a load error, then the queue", async () => {
    stubFetch({ [LIST]: [{ status: 500, body: { error: "internal error" } }, list(pending())] });
    render(<ApprovalQueue />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Could not load the approval queue");
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await table()).toBeInTheDocument();
  });
});
