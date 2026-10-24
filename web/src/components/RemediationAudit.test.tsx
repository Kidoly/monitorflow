import { act, render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { REFRESH_MS } from "@/lib/usePolledData";
import { stubFetch } from "@/test/fetch";
import { SERVER_ID, SITE_ID } from "@/test/incidents";
import { setUrl } from "@/test/navigation";
import { CUSTOMER_ID, INCIDENT_ID, event } from "@/test/remediation";

import { PAGE_SIZE, RemediationAuditView } from "./RemediationAudit";

vi.mock("next/navigation", () => import("@/test/navigation"));

const NOW = new Date("2026-10-19T12:00:00Z");
const SERVERS = {
  "GET /api/servers": {
    status: 200,
    body: { servers: [{ id: SERVER_ID, hostname: "web-1", site_id: SITE_ID, customer_id: CUSTOMER_ID }], next_cursor: null },
  },
};
const events = (list: ReturnType<typeof event>[], next_cursor: string | null = null) => ({
  status: 200,
  body: { events: list, next_cursor },
});
const approval = event({
  seq: 6,
  kind: "approval",
  actor_kind: "user",
  actor_id: "u1",
  actor_name: "dave",
  payload: { command: "systemctl reload nginx" },
});
const result = event({
  seq: 9,
  kind: "command_result",
  actor_kind: "peer",
  actor_id: "agent-web-1",
  payload: { exit_code: 0, duration_ms: 120, error: null },
});
const detail = (valid: boolean) => ({
  status: 200,
  body: {
    id: INCIDENT_ID,
    hostname: "web-1",
    metric: "monitorflow_disk_used_ratio",
    instance: "/var",
    events: [],
    chain: valid
      ? { valid: true, events: 9, broken_at: null, reason: null, head_hash: "0123456789abcdef" }
      : { valid: false, events: 9, broken_at: 4, reason: "hash", head_hash: null },
  },
});

const table = () => screen.findByRole("table", { name: "Remediation events" });

describe("RemediationAudit", () => {
  beforeEach(() => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(NOW);
    setUrl("/remediation/audit/");
  });
  afterEach(() => vi.useRealTimers());

  it("lists events newest first with who did what", async () => {
    stubFetch({ ...SERVERS, [`GET /api/remediation/events?limit=${PAGE_SIZE}`]: events([result, approval]) });
    render(<RemediationAuditView />);
    const rows = within(await table()).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(rows[1]).toHaveTextContent(/agent agent-web-1\s*Command result\s*web-1paris · Disk used \/var, event 9\s*Exit code 0, 120 ms$/);
    expect(rows[2]).toHaveTextContent(/dave\s*Approval\s*web-1.*Approved: systemctl reload nginx$/);
    expect(within(rows[2]!).getByRole("link", { name: "Disk used /var, event 6" })).toHaveAttribute(
      "href",
      expect.stringMatching(new RegExp(`^/remediation/audit/?\\?incident=${INCIDENT_ID}$`)),
    );
    expect(screen.getByText(/Choose an incident in the Target column to verify its chain/)).toBeInTheDocument();
    expect(screen.getByText("Showing 2 events · 50 per page")).toBeInTheDocument();
  });

  it("verifies the chain of the incident it is filtered to", async () => {
    setUrl(`/remediation/audit/?incident=${INCIDENT_ID}`);
    stubFetch({
      ...SERVERS,
      [`GET /api/remediation/events?incident_id=${INCIDENT_ID}&limit=${PAGE_SIZE}`]: events([approval]),
      [`GET /api/remediation/incidents/${INCIDENT_ID}`]: [detail(true), detail(false)],
    });
    render(<RemediationAuditView />);
    expect(await screen.findByText(/Hash chain verified: 9 events, head 0123456789ab\./)).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Show every incident" })).toBeInTheDocument();
    await act(() => vi.advanceTimersByTimeAsync(REFRESH_MS));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Hash chain broken at event 4: an event whose content does not match its hash/,
    );
  });

  it("says when the chain cannot be read", async () => {
    setUrl(`/remediation/audit/?incident=${INCIDENT_ID}`);
    stubFetch({
      ...SERVERS,
      [`GET /api/remediation/events?incident_id=${INCIDENT_ID}&limit=${PAGE_SIZE}`]: events([]),
      [`GET /api/remediation/incidents/${INCIDENT_ID}`]: { status: 404, body: { error: "not found" } },
    });
    render(<RemediationAuditView />);
    expect(await screen.findByText(/This incident's chain could not be verified/)).toBeInTheDocument();
    expect(screen.getByText("No events match these filters.")).toBeInTheDocument();
  });

  it("passes filters and the scope to the API", async () => {
    setUrl(`/remediation/audit/?customer=${CUSTOMER_ID}&kind=approval&actor=user`);
    stubFetch({
      ...SERVERS,
      [`GET /api/remediation/events?kind=approval&actor_kind=user&customer_id=${CUSTOMER_ID}&limit=${PAGE_SIZE}`]: events([approval]),
      [`GET /api/remediation/events?server_id=${SERVER_ID}&kind=approval&actor_kind=user&customer_id=${CUSTOMER_ID}&limit=${PAGE_SIZE}`]:
        events([]),
    });
    render(<RemediationAuditView />);
    expect(await table()).toBeInTheDocument();
    expect(screen.getByRole("combobox", { name: "Event" })).toHaveValue("approval");
    expect(screen.getByRole("combobox", { name: "Actor" })).toHaveValue("user");
    await screen.findByRole("option", { name: "Server: web-1" });
    await userEvent.selectOptions(screen.getByRole("combobox", { name: "Server" }), SERVER_ID);
    expect(await screen.findByText("No events match these filters.")).toBeInTheDocument();
  });

  it("loads more events", async () => {
    const page = (n: number) => Array.from({ length: n }, (_, i) => event({ seq: n - i }));
    stubFetch({
      ...SERVERS,
      [`GET /api/remediation/events?limit=${PAGE_SIZE}`]: events(page(PAGE_SIZE), "c1"),
      [`GET /api/remediation/events?limit=${2 * PAGE_SIZE}`]: events(page(PAGE_SIZE + 5)),
    });
    render(<RemediationAuditView />);
    await userEvent.click(await screen.findByRole("button", { name: "Load more" }));
    await vi.waitFor(() => expect(screen.getAllByRole("row")).toHaveLength(PAGE_SIZE + 6));
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("says when nothing ran yet", async () => {
    stubFetch({ ...SERVERS, [`GET /api/remediation/events?limit=${PAGE_SIZE}`]: events([]) });
    render(<RemediationAuditView />);
    expect(await screen.findByText("No remediation has run yet.")).toBeInTheDocument();
  });
});
