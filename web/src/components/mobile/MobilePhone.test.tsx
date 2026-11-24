import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { Phone, PhoneList } from "@/lib/api";
import { stubFetch } from "@/test/fetch";
import { router } from "@/test/navigation";

import { MobilePhone } from "./MobilePhone";

vi.mock("next/navigation", () => import("@/test/navigation"));

const PHONE: Phone = {
  id: "p1",
  name: "Pixel 8",
  created_at: "2026-11-20T10:00:00Z",
  last_seen_at: "2026-11-24T08:00:00Z",
  push: false,
  push_host: null,
  push_sent_at: null,
  push_error: null,
  this_phone: true,
};
const list = (phones: Phone[]): PhoneList => ({ phones, push_key: "BA", session_days: 90, idle_days: 30 });

describe("MobilePhone", () => {
  beforeEach(() => {
    vi.stubGlobal("navigator", { ...navigator, userAgent: "Mozilla/5.0 (Linux; Android 14; Pixel 8) Mobile" });
  });
  afterEach(() => vi.unstubAllGlobals());

  it("adds this phone with a name taken from it", async () => {
    const fetchMock = stubFetch({
      "GET /api/me/phones": [{ status: 200, body: list([]) }, { status: 200, body: list([PHONE]) }],
      "POST /api/me/phones": { status: 201, body: PHONE },
    });
    render(<MobilePhone />);
    expect(await screen.findByText(/keeps you signed in on it for 90 days/)).toBeInTheDocument();
    expect(screen.getByRole("textbox", { name: "Name" })).toHaveValue("Pixel 8");
    await userEvent.setup().click(screen.getByRole("button", { name: "Add this phone" }));
    expect(await screen.findByText("This phone is added.")).toBeInTheDocument();
    const call = fetchMock.mock.calls.find(([url, init]) => url === "/api/me/phones" && init?.method === "POST");
    expect(JSON.parse(String(call?.[1]?.body))).toEqual({ name: "Pixel 8" });
    expect(await screen.findByText(/You stay signed in here for 90 days, or until it goes unused for 30 days\./)).toBeInTheDocument();
  });

  it("says what blocks notifications", async () => {
    stubFetch({ "GET /api/me/phones": { status: 200, body: list([PHONE]) } });
    vi.stubGlobal("navigator", { userAgent: "Mozilla/5.0 (Linux; Android 14) Mobile", serviceWorker: {} });
    vi.stubGlobal("Notification", { permission: "denied" });
    vi.stubGlobal("PushManager", function PushManager() {});
    render(<MobilePhone />);
    expect(await screen.findByText(/Notifications are blocked for MonitorFlow/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Turn on notifications" })).not.toBeInTheDocument();
  });

  it("tests and removes a phone, signing this one out", async () => {
    const on = { ...PHONE, push: true, push_host: "fcm.example.net", push_error: "push.example.net answered 403 Forbidden." };
    const other = { ...PHONE, id: "p2", name: "Old iPhone", this_phone: false };
    const fetchMock = stubFetch({
      "GET /api/me/phones": { status: 200, body: list([on, other]) },
      "POST /api/me/phones/p1/test": { status: 200, body: { ok: false, error: "The push service refused it." } },
      "DELETE /api/me/phones/p2": { status: 204 },
      "DELETE /api/me/phones/p1": { status: 204 },
    });
    render(<MobilePhone />);
    const user = userEvent.setup();
    expect(await screen.findByText("push.example.net answered 403 Forbidden.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Send test" }));
    expect(await screen.findByText("The push service refused it.")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "Remove Old iPhone" }));
    expect(screen.getByText("Remove Old iPhone? It is signed out and gets no more notifications.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Remove" }));
    expect(await screen.findByText("Old iPhone is removed.")).toBeInTheDocument();
    expect(router.replace).not.toHaveBeenCalled();

    await user.click(screen.getByRole("button", { name: "Remove Pixel 8" }));
    await user.click(screen.getByRole("button", { name: "Remove" }));
    await vi.waitFor(() => expect(router.replace).toHaveBeenCalledWith("/login/"));
    expect(fetchMock.mock.calls.some(([url, init]) => url === "/api/me/phones/p1" && init?.method === "DELETE")).toBe(true);
  });
});
