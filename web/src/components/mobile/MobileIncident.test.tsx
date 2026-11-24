import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { stubFetch } from "@/test/fetch";
import { SERVER_ID, incident } from "@/test/incidents";

import { MobileIncident } from "./MobileIncident";

vi.mock("next/navigation", () => import("@/test/navigation"));

const OPEN = { ...incident({ can_acknowledge: true }), events: [] };
const URL_OF = `/api/incidents/${OPEN.id}`;
const SENSORS = `/api/servers/${SERVER_ID}/sensors`;

describe("MobileIncident", () => {
  afterEach(() => window.history.replaceState(null, "", "/"));

  it("acknowledges with one tap", async () => {
    window.history.replaceState(null, "", `/mobile/incident/?id=${OPEN.id}`);
    const acknowledged = { ...OPEN, acknowledged_by: "alice", acknowledged_at: "2026-10-19T12:00:00Z" };
    const fetchMock = stubFetch({
      [`GET ${URL_OF}`]: [{ status: 200, body: OPEN }, { status: 200, body: acknowledged }],
      "POST /api/incidents/acknowledge": { status: 200, body: { acknowledged: [OPEN.id] } },
    });
    render(<MobileIncident />);
    await userEvent.setup().click(await screen.findByRole("button", { name: "Acknowledge" }));
    expect(await screen.findByText("Acknowledged: nobody else is told next.")).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url]) => url === "/api/incidents/acknowledge");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ incident_ids: [OPEN.id], note: "" });
    expect(await screen.findByText("Acknowledged by alice")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Acknowledge" })).not.toBeInTheDocument();
  });

  it("silences the check for the chosen time, opened from a notification", async () => {
    window.history.replaceState(null, "", `/mobile/incident/?id=${OPEN.id}&silence=1`);
    const fetchMock = stubFetch({
      [`GET ${URL_OF}`]: { status: 200, body: { ...OPEN, can_acknowledge: false } },
      [`GET ${SENSORS}`]: { status: 200, body: { sensors: [{ name: "disk", instance: "", kind: "disk", params: {} }] } },
      "POST /api/silences": { status: 201, body: {} },
    });
    render(<MobileIncident />);
    expect(await screen.findByRole("heading", { name: "Silence for" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Acknowledge" })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "This check" })).toHaveAttribute("aria-pressed", "true");
    await userEvent.setup().click(screen.getByRole("button", { name: /^1\sh$/ }));
    expect(await screen.findByText(/^This check of web-1 is silenced for 1\sh\.$/)).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url]) => url === "/api/silences");
    const body = JSON.parse(String(call?.[1]?.body));
    expect(body).toMatchObject({
      scope: "server",
      target_id: SERVER_ID,
      checks: [{ name: "disk", instance: "" }],
      reason: "Silenced from a phone",
    });
    const minutes = (Date.parse(body.ends_at) - Date.now()) / 60_000;
    expect(minutes).toBeGreaterThan(59);
    expect(minutes).toBeLessThanOrEqual(60);
  });

  it("silences the whole server when asked", async () => {
    window.history.replaceState(null, "", `/mobile/incident/?id=${OPEN.id}`);
    const fetchMock = stubFetch({
      [`GET ${URL_OF}`]: { status: 200, body: OPEN },
      "POST /api/silences": { status: 201, body: {} },
    });
    render(<MobileIncident />);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Silence…" }));
    await user.click(screen.getByRole("button", { name: "Whole server" }));
    await user.click(screen.getByRole("button", { name: /^4\sh$/ }));
    expect(await screen.findByText(/^web-1 is silenced for 4\sh\.$/)).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url]) => url === "/api/silences");
    expect(JSON.parse(String(call?.[1]?.body)).checks).toBeUndefined();
  });

  it("offers nothing on a resolved incident", async () => {
    window.history.replaceState(null, "", `/mobile/incident/?id=${OPEN.id}`);
    stubFetch({ [`GET ${URL_OF}`]: { status: 200, body: { ...OPEN, state: "resolved", resolved_at: "2026-10-19T12:00:00Z" } } });
    render(<MobileIncident />);
    expect(await screen.findAllByText("Resolved")).not.toHaveLength(0);
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
