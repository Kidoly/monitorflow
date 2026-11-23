import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { ScriptRunDetail } from "@/lib/api";
import { As, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { ScriptRunPage } from "./ScriptRunPage";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ID = "7a8b9c0d-1e2f-4a3b-8c4d-5e6f7a8b9c0d";

function detail(overrides: Partial<ScriptRunDetail["run"]> = {}): ScriptRunDetail {
  return {
    run: {
      id: ID, script_id: "s1", script_name: "clear-cache", version: 2, interpreter: "sh", customer_id: "c1",
      parameters: { days: "14" }, target: { kind: "site", site_id: "site-1" }, requested_by: "u2", requested_by_name: "bob",
      rule_id: null, rule_name: null, trigger: null, state: "awaiting_approval", tier: "high_impact",
      rule: { source: "builtin", id: "default.unknown" }, reason: "line 2: unknown", scheduled_at: "2026-12-04T22:00:00Z",
      created_at: "2026-12-04T10:00:00Z", expires_at: "2026-12-05T10:00:00Z", decided_by: null, decided_by_name: null,
      decided_at: null, decision_reason: null, dispatched_at: null,
      counts: { servers: 1, pending: 1, succeeded: 0, failed: 0 }, can_approve: true, can_reject: true, can_cancel: true,
      ...overrides,
    },
    body: "df -h\nfind /var/cache -delete\n",
    servers: [
      { server_id: "srv1", hostname: "web-1", site_id: "site-1", status: "waiting", command_id: null, exit_code: null,
        stdout: null, stderr: null, error: null, started_at: null, finished_at: null },
    ],
  };
}

afterEach(() => vi.unstubAllGlobals());

describe("script run page", () => {
  it("shows the run and approves it", async () => {
    const done = detail({
      state: "dispatched", can_approve: false, can_reject: false, can_cancel: false, dispatched_at: "2026-12-04T22:00:01Z",
      counts: { servers: 1, pending: 0, succeeded: 1, failed: 0 },
    });
    done.servers[0] = { ...done.servers[0]!, status: "succeeded", exit_code: 0, stdout: "all good\n", finished_at: "2026-12-04T22:00:03Z" };
    const fetchMock = stubFetch({
      [`GET /api/remediation/script-runs/${ID}`]: [{ status: 200, body: detail() }, { status: 200, body: done }],
      [`POST /api/remediation/script-runs/${ID}/approve`]: { status: 200, body: done },
    });
    render(
      <As user={me({ role: "administrator", customer_id: null, site_id: null })}>
        <ScriptRunPage id={ID} />
      </As>,
    );
    expect(await screen.findByRole("heading", { name: "Run of clear-cache" })).toBeInTheDocument();
    expect(screen.getByText("days=14")).toBeInTheDocument();
    expect(screen.getByText("Awaiting approval")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(await screen.findByText("all good")).toBeInTheDocument();
    expect(fetchMock.mock.calls.some((c) => c[1]?.method === "POST")).toBe(true);
  });

  it("says when the run does not exist", async () => {
    stubFetch({ [`GET /api/remediation/script-runs/${ID}`]: { status: 404, body: { error: "not found" } } });
    render(<ScriptRunPage id={ID} />);
    expect(await screen.findByText("Run not found")).toBeInTheDocument();
  });
});
