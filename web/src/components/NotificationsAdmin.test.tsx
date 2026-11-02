import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { NotificationChannel } from "@/lib/api";
import { As, customerAdmin, me, siteAdmin, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { NotificationsPage, destination, newSecret } from "./NotificationsAdmin";

vi.mock("next/navigation", () => import("@/test/navigation"));

const globalAdmin = me({ role: "administrator", customer_id: null, site_id: null });
const CHANNELS = "/api/notifications/channels";
const customers = { status: 200, body: { customers: [{ id: "c1", name: "default", site_count: 1 }] } };
const noSites = { status: 200, body: { sites: [] } };
const site = (id: string, name: string, customer_id: string, customer_name: string) => ({
  id,
  name,
  customer_id,
  customer_name,
});
const twoCustomers = {
  status: 200,
  body: {
    customers: [
      { id: "c1", name: "acme", site_count: 2 },
      { id: "c2", name: "globex", site_count: 1 },
    ],
  },
};
const threeSites = {
  status: 200,
  body: {
    sites: [
      site("site-1", "paris", "c1", "acme"),
      site("site-2", "lyon", "c1", "acme"),
      site("site-3", "berlin", "c2", "globex"),
    ],
  },
};

function hook(overrides: Partial<NotificationChannel> = {}): NotificationChannel {
  return {
    id: "n1",
    kind: "webhook",
    settings: { url: "https://hooks.slack.com/services/T0/B0/X" },
    has_secret: true,
    customer_id: null,
    customer_name: null,
    site_id: null,
    site_name: null,
    created_at: "2026-10-29T10:00:00Z",
    last_sent_at: null,
    last_error: null,
    last_error_at: null,
    ...overrides,
  } as NotificationChannel;
}

const page = (user = globalAdmin) =>
  render(
    <As user={user}>
      <NotificationsPage />
    </As>,
  );

const posted = (fetchMock: ReturnType<typeof stubFetch>) =>
  JSON.parse(String(fetchMock.mock.calls.find(([url, init]) => url === CHANNELS && init?.method === "POST")?.[1]?.body));

describe("NotificationsPage", () => {
  beforeEach(() => vi.unstubAllGlobals());

  it("is for administrators", () => {
    page(viewer);
    expect(screen.getByText("Notifications need an administrator")).toBeInTheDocument();
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
      "GET /api/sites": noSites,
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
    expect(posted(fetchMock)).toEqual({
      kind: "email",
      settings: { to: ["a@example.com"], host: "smtp.example.com", port: 587, username: "ops@example.com", from: "" },
      secret: "hunter2",
      customer_id: null,
      site_id: null,
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
      "GET /api/sites": noSites,
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
      "GET /api/sites": noSites,
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

describe("Notify about", () => {
  beforeEach(() => vi.unstubAllGlobals());

  const empty = { [`GET ${CHANNELS}`]: { status: 200, body: { channels: [] } } };

  it("does not ask with one customer and one site", async () => {
    stubFetch({ ...empty, "GET /api/customers": customers, "GET /api/sites": {
      status: 200, body: { sites: [site("site-1", "paris", "c1", "default")] } } });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add webhook" }));
    await screen.findByLabelText(/^URL/);
    expect(screen.queryByLabelText(/Notify about/)).not.toBeInTheDocument();
  });

  it("offers sites, not customers, with one customer", async () => {
    stubFetch({ ...empty, "GET /api/customers": customers, "GET /api/sites": {
      status: 200,
      body: { sites: [site("site-1", "paris", "c1", "default"), site("site-2", "lyon", "c1", "default")] },
    } });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add webhook" }));
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "paris" });
    expect(within(scope).getAllByRole("option").map((o) => o.textContent)).toEqual(["Everything", "paris", "lyon"]);
  });

  it("says what each choice means and saves a site", async () => {
    const saved = hook({ id: "n2", site_id: "site-2", site_name: "lyon", customer_id: "c1", customer_name: "acme" });
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: [{ status: 200, body: { channels: [] } }, { status: 200, body: { channels: [saved] } }],
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n2/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Add webhook" }));
    await user.type(screen.getByLabelText(/^URL/), "https://hooks.example/x");
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "globex / berlin" });
    expect(within(scope).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "Everything",
      "All of acme",
      "acme / paris",
      "acme / lyon",
      "All of globex",
      "globex / berlin",
    ]);
    expect(screen.getByText(/news about the central server/)).toBeInTheDocument();
    await user.selectOptions(scope, "All of acme");
    expect(screen.getByText("Alerts from acme and all its sites. Nothing from other customers.")).toBeInTheDocument();
    await user.selectOptions(scope, "acme / lyon");
    expect(screen.getByText("Alerts from lyon only.")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save and send test" }));
    await screen.findByText("Test sent");
    expect(posted(fetchMock)).toMatchObject({ customer_id: null, site_id: "site-2" });
    expect(within(screen.getByRole("table")).getByText("acme / lyon")).toBeInTheDocument();
  });

  it("offers a customer administrator its own customer and sites, starting with the customer", async () => {
    const saved = hook({ id: "n2", customer_id: "c1", customer_name: "acme" });
    const fetchMock = stubFetch({
      ...empty,
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`POST ${CHANNELS}`]: { status: 201, body: saved },
      [`POST ${CHANNELS}/n2/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page(customerAdmin);
    await user.click(await screen.findByRole("button", { name: "Add webhook" }));
    await user.type(screen.getByLabelText(/^URL/), "https://hooks.example/x");
    const scope = await screen.findByRole("combobox", { name: /Notify about/ });
    await within(scope).findByRole("option", { name: "acme / paris" });
    expect(within(scope).getAllByRole("option").map((o) => o.textContent)).toEqual([
      "All of acme",
      "acme / paris",
      "acme / lyon",
    ]);
    await user.click(screen.getByRole("button", { name: "Save and send test" }));
    await waitFor(() => expect(posted(fetchMock)).toMatchObject({ customer_id: "c1", site_id: null }));
  });

  it("gives a site administrator its site without asking", async () => {
    stubFetch({
      ...empty,
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] } },
      "GET /api/sites": { status: 200, body: { sites: [site("site-1", "paris", "c1", "acme")] } },
    });
    const user = userEvent.setup();
    page(siteAdmin);
    await user.click(await screen.findByRole("button", { name: "Add webhook" }));
    expect(await screen.findByText("Alerts from paris only.")).toBeInTheDocument();
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
  });

  it("does not let a customer administrator save before knowing where", async () => {
    stubFetch({ ...empty, "GET /api/customers": { status: 500, body: {} }, "GET /api/sites": { status: 500, body: {} } });
    const user = userEvent.setup();
    page(customerAdmin);
    await user.click(await screen.findByRole("button", { name: "Add webhook" }));
    expect(screen.getByRole("button", { name: "Save and send test" })).toBeDisabled();
  });

  it("keeps what an edited channel covers", async () => {
    const scoped = hook({ customer_id: "c2", customer_name: "globex" });
    const fetchMock = stubFetch({
      [`GET ${CHANNELS}`]: { status: 200, body: { channels: [scoped] } },
      "GET /api/customers": twoCustomers,
      "GET /api/sites": threeSites,
      [`PUT ${CHANNELS}/n1`]: { status: 200, body: scoped },
      [`POST ${CHANNELS}/n1/test`]: { status: 200, body: { ok: true, error: null } },
    });
    const user = userEvent.setup();
    page();
    await user.click(await screen.findByRole("button", { name: "Edit" }));
    expect(await screen.findByRole("combobox", { name: /Notify about/ })).toHaveDisplayValue("All of globex");
    await user.click(screen.getByRole("button", { name: "Save and send test" }));
    await screen.findByText("Test sent");
    const put = fetchMock.mock.calls.find(([, init]) => init?.method === "PUT");
    expect(JSON.parse(String(put?.[1]?.body))).toMatchObject({ customer_id: "c2", site_id: null });
  });
});

describe("destination", () => {
  it("names where a channel sends", () => {
    expect(destination(hook())).toBe("Webhook to hooks.slack.com");
    expect(newSecret()).not.toBe(newSecret());
  });
});
