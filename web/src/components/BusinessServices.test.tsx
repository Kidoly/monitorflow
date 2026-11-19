import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { BusinessService, NotificationChannel, ReportSchedule } from "@/lib/api";
import { As, me, single, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { BusinessServicesSection, describeRule, serviceRequestOf } from "./BusinessServices";
import { ReportSchedulesSection, channelsFor, scheduleRequestOf } from "./ReportSchedules";

vi.mock("next/navigation", () => import("@/test/navigation"));

const ok = (body: unknown) => ({ status: 200, body });
const operator = single(me({ role: "operator", customer_id: null, site_id: null }));
const administrator = single(me({ role: "administrator", customer_id: null, site_id: null }));

const service: BusinessService = {
  id: "b1",
  customer_id: "c1",
  customer_name: "default",
  name: "Website",
  description: "",
  rule: "at_least",
  at_least: 2,
  target: 99.9,
  members: [
    { server_id: "s1", server_name: "web-1", device: false, check: null },
    { server_id: "s2", server_name: "web-2", device: false, check: null },
    { server_id: "s3", server_name: "web-3", device: false, check: { name: "port_443", instance: "" } },
  ],
  created_at: "2026-11-18T09:00:00Z",
  updated_at: "2026-11-18T09:00:00Z",
  can_change: true,
};

const places = {
  "GET /api/customers": ok({ customers: [{ id: "c1", name: "default", site_count: 1 }] }),
  "GET /api/servers": ok({
    servers: [
      { id: "s1", hostname: "mail-1", site_id: "site-1", customer_id: "c1" },
      { id: "s2", hostname: "mail-2", site_id: "site-1", customer_id: "c1" },
    ],
    next_cursor: null,
  }),
  "GET /api/servers/s1/sensors": ok({
    server_id: "s1",
    site_id: "site-1",
    sensors: [{ origin: "server", entry_id: "e1", kind: "systemd", name: "service_postfix", instance: "", interval_ms: 60_000, timeout_ms: null, params: {} }],
  }),
};

const bodyOf = (fetchMock: ReturnType<typeof stubFetch>, method: string) =>
  JSON.parse(String(fetchMock.mock.calls.find((c) => c[1]?.method === method)?.[1]?.body)) as Record<string, unknown>;

afterEach(() => {
  vi.unstubAllGlobals();
  setActiveLocale("en");
});

describe("business services", () => {
  const draft = { customerId: "c1", name: " Email ", description: "", atLeast: false, count: 1, target: "99,9", members: [{ serverId: "s1", check: "" }] };

  it("turn a form into a request, or say why not", () => {
    expect(serviceRequestOf(draft, false)).toEqual({
      name: "Email",
      description: "",
      rule: "all",
      at_least: null,
      target: 99.9,
      members: [{ server_id: "s1", check: null }],
    });
    expect(serviceRequestOf({ ...draft, members: [{ serverId: "s1", check: "port_443\t" }] }, true)).toMatchObject({
      customer_id: "c1",
      members: [{ server_id: "s1", check: { name: "port_443", instance: "" } }],
    });
    expect(serviceRequestOf({ ...draft, name: " " }, false)).toBe("Give the service a name.");
    expect(serviceRequestOf({ ...draft, target: "120" }, false)).toBe("The target is a percentage from 50 to 100, like 99.9.");
    expect(serviceRequestOf({ ...draft, atLeast: true, count: 2 }, false)).toBe("Say how many must be up: 1 to 1.");
    expect(serviceRequestOf({ ...draft, members: [{ serverId: "", check: "" }] }, false)).toBe("Choose at least one server or device.");
  });

  it("say their rule in words", () => {
    expect(describeRule(service)).toBe("At least 2 of 3");
    expect(describeRule({ ...service, rule: "all", at_least: null })).toBe("All of them");
    setActiveLocale("fr");
    expect(describeRule(service)).toBe("Au moins 2 sur 3");
  });

  it("are made from the Reports page", async () => {
    const fetchMock = stubFetch({
      "GET /api/business-services": [ok({ services: [] }), ok({ services: [{ ...service, name: "Email" }] })],
      ...places,
      "POST /api/business-services": ok(service),
    });
    render(
      <As user={operator}>
        <BusinessServicesSection />
      </As>,
    );
    expect(await screen.findByText(/No business service yet/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "New business service" }));
    const form = await screen.findByRole("form", { name: "New business service" });
    // One customer: no customer to choose.
    expect(within(form).queryByLabelText("Customer")).not.toBeInTheDocument();
    await userEvent.type(within(form).getByLabelText("Name"), "Email");
    await userEvent.selectOptions(await within(form).findByLabelText("Server or device of member 1"), "s1");
    await userEvent.selectOptions(within(form).getByLabelText("Check of member 1"), await within(form).findByRole("option", { name: "service_postfix" }));
    await userEvent.type(within(form).getByLabelText(/Target availability/), "99.9");
    await userEvent.click(within(form).getByRole("button", { name: "Add the business service" }));
    expect(bodyOf(fetchMock, "POST")).toEqual({
      name: "Email",
      description: "",
      rule: "all",
      at_least: null,
      target: 99.9,
      members: [{ server_id: "s1", check: { name: "service_postfix", instance: "" } }],
    });
    expect(await screen.findByRole("cell", { name: "Email" })).toBeInTheDocument();
  });

  it("are hidden from a viewer when there are none", async () => {
    stubFetch({ "GET /api/business-services": ok({ services: [] }) });
    const { container } = render(
      <As user={viewer}>
        <BusinessServicesSection />
      </As>,
    );
    await vi.waitFor(() => expect(container).toBeEmptyDOMElement());
  });
});

const channel = (overrides: Partial<NotificationChannel>): NotificationChannel =>
  ({
    id: "n1",
    kind: "email",
    settings: { host: "smtp.example.com", port: 587, username: "", from: "", to: ["ops@example.com"] },
    has_secret: false,
    customer_id: null,
    customer_name: null,
    site_id: null,
    site_name: null,
    created_at: "2026-11-01T00:00:00Z",
    last_sent_at: null,
    last_error: null,
    last_error_at: null,
    ...overrides,
  }) as NotificationChannel;

const schedule: ReportSchedule = {
  id: "r1",
  customer_id: null,
  customer_name: null,
  frequency: "monthly",
  time_zone: "Europe/Paris",
  language: "en",
  pdf: true,
  csv: true,
  channel_id: "n1",
  channel_host: "smtp.example.com",
  recipients: [],
  channel_recipients: ["ops@example.com"],
  enabled: true,
  last_period_end: null,
  last_sent_at: null,
  last_error: "No answer from smtp.example.com:587 within 10 seconds.",
  last_error_at: "2026-11-01T05:00:00Z",
  next_from: "2026-10-31T23:00:00Z",
  next_to: "2026-11-30T23:00:00Z",
  next_send_at: "2026-12-01T05:00:00Z",
  created_at: "2026-10-20T09:00:00Z",
};

describe("scheduled reports", () => {
  it("send through email channels of the customer as a whole, or of everything", () => {
    const channels = [
      channel({ id: "all" }),
      channel({ id: "acme", customer_id: "c1" }),
      channel({ id: "site", customer_id: "c1", site_id: "site-1" }),
      channel({ id: "hook", kind: "webhook", settings: { origin: "https://x.example" } } as Partial<NotificationChannel>),
    ];
    expect(channelsFor(channels, "c1").map((c) => c.id)).toEqual(["all", "acme"]);
    expect(channelsFor(channels, "").map((c) => c.id)).toEqual(["all"]);
    const draft = { customerId: "", frequency: "monthly" as const, zone: "UTC", language: "fr" as const, pdf: true, csv: false, channelId: "all", recipients: "a@x.example, b@x.example;c@x.example", enabled: true };
    expect(scheduleRequestOf(draft)).toMatchObject({ customer_id: null, recipients: ["a@x.example", "b@x.example", "c@x.example"], pdf: true, csv: false });
    expect(scheduleRequestOf({ ...draft, channelId: "" })).toBe("Choose the email channel to send it through.");
    expect(scheduleRequestOf({ ...draft, pdf: false })).toBe("Attach the PDF, the CSV or both.");
  });

  it("are for administrators only", () => {
    const fetchMock = stubFetch({});
    const { container } = render(
      <As user={operator}>
        <ReportSchedulesSection />
      </As>,
    );
    expect(container).toBeEmptyDOMElement();
    expect(fetchMock).not.toHaveBeenCalled();
  });

  it("say how the last sending went, and send now", async () => {
    const fetchMock = stubFetch({
      "GET /api/report-schedules": ok({ schedules: [schedule] }),
      "POST /api/report-schedules/r1/send": ok({ ok: true, error: null, from: schedule.next_from, to: schedule.next_to, recipients: ["ops@example.com"] }),
    });
    render(
      <As user={administrator}>
        <ReportSchedulesSection />
      </As>,
    );
    expect(await screen.findByText(/Not sent: No answer from smtp.example.com/)).toBeInTheDocument();
    expect(screen.getByRole("cell", { name: "ops@example.com" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Actions for the scheduled report" }));
    await userEvent.click(screen.getByRole("menuitem", { name: "Send now" }));
    expect(await screen.findByText("Sent to ops@example.com.")).toBeInTheDocument();
    expect(fetchMock.mock.calls.some((c) => c[1]?.method === "POST")).toBe(true);
  });

  it("are made with the channel chosen", async () => {
    const fetchMock = stubFetch({
      "GET /api/report-schedules": ok({ schedules: [] }),
      "GET /api/customers": ok({ customers: [{ id: "c1", name: "default", site_count: 1 }] }),
      "GET /api/notifications/channels": ok({ channels: [channel({})] }),
      "POST /api/report-schedules": ok(schedule),
    });
    render(
      <As user={administrator}>
        <ReportSchedulesSection />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "New scheduled report" }));
    const form = await screen.findByRole("form", { name: "New scheduled report" });
    await userEvent.selectOptions(await within(form).findByLabelText(/Sent through/), "n1");
    await userEvent.click(within(form).getByRole("button", { name: "Add the scheduled report" }));
    expect(bodyOf(fetchMock, "POST")).toMatchObject({ customer_id: null, frequency: "monthly", channel_id: "n1", recipients: [], pdf: true, csv: true });
  });
});
