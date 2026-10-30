import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { NotificationChannel } from "@/lib/api";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { NotificationsPage, destination, newSecret } from "./NotificationsAdmin";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const CHANNELS = "/api/notifications/channels";
const customers = { status: 200, body: { customers: [{ id: "c1", name: "default", site_count: 1 }] } };

function hook(overrides: Partial<NotificationChannel> = {}): NotificationChannel {
  return {
    id: "n1",
    kind: "webhook",
    settings: { url: "https://hooks.slack.com/services/T0/B0/X" },
    has_secret: true,
    customer_id: null,
    customer_name: null,
    created_at: "2026-10-29T10:00:00Z",
    last_sent_at: null,
    last_error: null,
    last_error_at: null,
    ...overrides,
  } as NotificationChannel;
}

const page = () =>
  render(
    <As user={globalAdmin}>
      <NotificationsPage />
    </As>,
  );

describe("NotificationsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("is for global administrators only", () => {
    render(
      <As user={customerAdmin}>
        <NotificationsPage />
      </As>,
    );
    expect(screen.getByText("Notifications need a global administrator")).toBeInTheDocument();
  });

  it("starts with the next action when nothing is set up", async () => {
    stubFetch({ [`GET ${CHANNELS}`]: { status: 200, body: { channels: [] } }, "GET /api/customers": customers });
    page();
    expect(await screen.findByText("No one is notified yet")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add email" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Add webhook" })).toBeInTheDocument();
  });

  it("saves an email channel, then sends a test and shows its result", async () => {
    const saved = {
      ...hook(),
      id: "n2",
      kind: "email",
      settings: { host: "smtp.example.com", port: 587, username: "ops@example.com", from: "", to: ["a@example.com"] },
    } as NotificationChannel;
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: [
        { status: 200, body: { channels: [] } },
        { status: 200, body: { channels: [saved] } },
      ],
      "GET /api/customers": customers,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n2/test`]: {
        status: 200,
        body: { ok: false, error: "smtp.example.com:587 refused the username or password (535 bad credentials)." },
      },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add email" }));
    await user.type(screen.getByLabelText("Send to"), "a@example.com");
    await user.type(screen.getByLabelText("Mail server"), "smtp.example.com");
    await user.type(screen.getByLabelText("Username"), "ops@example.com");
    await user.type(screen.getByLabelText(/^Password/), "hunter2");
    await user.click(screen.getByRole("button", { name: "Save and send test" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("refused the username or password");
    const body = JSON.parse(
      String(fetchMock.mock.calls.find(([url, init]) => url === CHANNELS && init?.method === "POST")?.[1]?.body),
    );
    expect(body).toEqual({
      kind: "email",
      settings: { to: ["a@example.com"], host: "smtp.example.com", port: 587, username: "ops@example.com", from: "" },
      secret: "hunter2",
      customer_id: null,
    });
    expect(within(screen.getByRole("table")).getByText(/Email to a@example.com/)).toBeInTheDocument();
  });

  it("shows each channel's state and tests it on demand", async () => {
    stubFetch({
      [`GET ${CHANNELS}`]: {
        status: 200,
        body: {
          channels: [
            hook(),
            hook({ id: "n3", settings: { url: "http://n8n.lan/x" }, last_error: "Could not connect to n8n.lan" }),
          ],
        },
      },
      "GET /api/customers": customers,
      [`POST ${CHANNELS}/n1/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page();
    expect(await screen.findByText("Could not connect to n8n.lan")).toBeInTheDocument();
    expect(screen.getByText("Nothing sent yet")).toBeInTheDocument();
    const row = screen.getByText("Webhook to hooks.slack.com").closest("tr")!;
    await user.click(within(row).getByRole("button", { name: "Send test" }));
    expect(await within(row).findByText("Test sent")).toBeInTheDocument();
  });

  it("keeps a saved secret unless a new one is typed, and generates one", async () => {
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [hook()] } },
      "GET /api/customers": customers,
      [`PUT ${CHANNELS}/n1`]: { status: 200, body: hook() },
      [`POST ${CHANNELS}/n1/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    const secret = screen.getByLabelText(/Signing secret/);
    expect(secret).toHaveValue("");
    expect(secret).toHaveAttribute("placeholder", "Saved; leave empty to keep it");
    await user.click(screen.getByRole("button", { name: "Save and send test" }));
    await screen.findByText("Test sent");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toMatchObject({ secret: null, settings: { url: "https://hooks.slack.com/services/T0/B0/X" } });

    await user.click(screen.getByRole("button", { name: "Edit" }));
    await user.click(screen.getByRole("button", { name: "Generate" }));
    expect(String((screen.getByLabelText(/Signing secret/) as HTMLInputElement).value)).toMatch(/^[0-9a-f]{64}$/);
  });
});

describe("destination", () => {
  it("names where a channel sends", () => {
    expect(destination(hook())).toBe("Webhook to hooks.slack.com");
    expect(newSecret()).not.toBe(newSecret());
  });
});
