import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";

import type { Silence } from "@/lib/api";
import { DEFAULT_REASON, tomorrowMorning } from "@/lib/silences";
import { As, customerAdmin, me, viewer } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { SilenceBar, SilencesPage, type SilenceTarget } from "./Silences";

vi.mock("next/navigation", () => import("@/test/navigation"));

const operator = me({ role: "operator", customer_id: null, site_id: "site-1" });
const target: SilenceTarget = { scope: "server", id: "s1", name: "web-1", customerId: "c1", siteId: "site-1" };
const LIST = "GET /api/silences?server_id=s1";
const none = { status: 200, body: { silences: [] } };

function silence(overrides: Partial<Silence> = {}): Silence {
  const hour = 3_600_000;
  return {
    id: "x1",
    scope: "server",
    target_id: "s1",
    target_name: "web-1",
    site_id: "site-1",
    customer_id: "c1",
    starts_at: new Date(Date.now() - hour).toISOString(),
    ends_at: new Date(Date.now() + hour).toISOString(),
    active: true,
    reason: "Kernel update",
    created_by_username: "alice",
    created_at: new Date(Date.now() - hour).toISOString(),
    can_end: true,
    ...overrides,
  };
}

const bodyOf = (call: Parameters<typeof fetch> | undefined) => JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;

afterEach(() => vi.useRealTimers());

describe("SilenceBar", () => {
  it("silences for an hour in one click, then says so", async () => {
    const fetchMock = stubFetch({
      [LIST]: [none, { status: 200, body: { silences: [silence({ reason: DEFAULT_REASON })] } }],
      "POST /api/silences": { status: 201, body: silence() },
    });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    const before = Date.now();
    await userEvent.click(await screen.findByRole("button", { name: "1 hour" }));
    expect(await screen.findByText(/Silenced until/)).toBeInTheDocument();
    const post = fetchMock.mock.calls.find((c) => c[1]?.method === "POST");
    const body = bodyOf(post);
    expect(body).toMatchObject({ scope: "server", target_id: "s1", reason: DEFAULT_REASON });
    expect(body.starts_at).toBeUndefined();
    expect(Date.parse(String(body.ends_at)) - before).toBeGreaterThan(3_590_000);
    expect(Date.parse(String(body.ends_at)) - before).toBeLessThan(3_700_000);
  });

  it("silences until tomorrow morning", async () => {
    const fetchMock = stubFetch({ [LIST]: none, "POST /api/silences": { status: 201, body: silence() } });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Until tomorrow, 8:00" }));
    const body = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "POST"));
    expect(body.ends_at).toBe(tomorrowMorning(new Date()).toISOString());
  });

  it("takes a custom reason and period", async () => {
    const fetchMock = stubFetch({ [LIST]: none, "POST /api/silences": { status: 201, body: silence() } });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Custom…" }));
    const reason = screen.getByLabelText("Why");
    await userEvent.clear(reason);
    await userEvent.type(reason, "Moving racks");
    await userEvent.type(screen.getByLabelText(/^From/), "2030-01-02T10:00");
    await userEvent.type(screen.getByLabelText(/^Until/), "2030-01-02T12:00");
    await userEvent.click(screen.getByRole("button", { name: "Silence web-1" }));
    const body = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "POST"));
    expect(body).toMatchObject({
      reason: "Moving racks",
      starts_at: new Date(2030, 0, 2, 10).toISOString(),
      ends_at: new Date(2030, 0, 2, 12).toISOString(),
    });
    expect(screen.queryByLabelText("Why")).not.toBeInTheDocument();
  });

  it("keeps the form open with the server's words when refused", async () => {
    stubFetch({
      [LIST]: none,
      "POST /api/silences": { status: 400, body: { error: "A silence lasts at most 30 days." } },
    });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Custom…" }));
    await userEvent.type(screen.getByLabelText(/^Until/), "2031-01-02T12:00");
    await userEvent.click(screen.getByRole("button", { name: "Silence web-1" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("at most 30 days");
    expect(screen.getByLabelText("Why")).toBeInTheDocument();
  });

  it("makes a silenced server plain, and lets it end early", async () => {
    const fetchMock = stubFetch({
      [LIST]: [{ status: 200, body: { silences: [silence()] } }, none],
      "POST /api/silences/x1/end": { status: 204 },
    });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    const banner = await screen.findByRole("status");
    expect(banner).toHaveTextContent(/Silenced until .*Nobody is told about web-1/);
    expect(banner).toHaveTextContent("Kernel update, by alice");
    expect(banner).toHaveTextContent("what is still open at the end is sent then");
    expect(screen.getByText("Silence longer")).toBeInTheDocument();
    await userEvent.click(within(banner).getByRole("button", { name: /End the silence/ }));
    expect(fetchMock.mock.calls.some((c) => c[1]?.method === "POST" && String(c[0]).endsWith("/x1/end"))).toBe(true);
    expect(await screen.findByText("Silence for")).toBeInTheDocument();
    expect(screen.queryByText(/Silenced until/)).not.toBeInTheDocument();
  });

  it("shows a silence of the whole site, and who may not end it", async () => {
    stubFetch({
      [LIST]: {
        status: 200,
        body: { silences: [silence({ scope: "site", target_name: "paris", can_end: false })] },
      },
    });
    render(
      <As user={viewer}>
        <SilenceBar target={target} />
      </As>,
    );
    expect(await screen.findByText(/With site paris/)).toBeInTheDocument();
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });

  it("announces an upcoming silence and lets it be called off", async () => {
    const soon = silence({ active: false, starts_at: new Date(Date.now() + 1_800_000).toISOString() });
    stubFetch({ [LIST]: { status: 200, body: { silences: [soon] } } });
    render(
      <As user={operator}>
        <SilenceBar target={target} />
      </As>,
    );
    expect(await screen.findByText(/Silence planned from/)).toBeInTheDocument();
    expect(screen.queryByText(/Silenced until/)).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: /End the silence/ })).toHaveTextContent("Call off");
  });

  it("offers nothing to another site's operator or a viewer", async () => {
    stubFetch({ [LIST]: none });
    const { rerender } = render(
      <As user={viewer}>
        <SilenceBar target={target} />
      </As>,
    );
    expect(screen.queryByRole("group")).not.toBeInTheDocument();
    rerender(
      <As user={operator}>
        <SilenceBar target={{ ...target, siteId: "site-2" }} />
      </As>,
    );
    expect(screen.queryByRole("group")).not.toBeInTheDocument();
  });
});

describe("SilencesPage", () => {
  const everything = silence({ id: "x2", scope: "all", target_id: null, target_name: null, site_id: null, customer_id: null, can_end: false });
  const choices = {
    "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }, { id: "c2", name: "globex", site_count: 1 }] } },
    "GET /api/sites": { status: 200, body: { sites: [{ id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme", server_count: 1 }] } },
    "GET /api/servers": { status: 200, body: { servers: [], next_cursor: null } },
  };

  it("lists active and upcoming silences with what they cover", async () => {
    stubFetch({
      "GET /api/silences": {
        status: 200,
        body: { silences: [silence(), everything, silence({ id: "x3", active: false, created_by_username: null })] },
      },
    });
    render(
      <As user={operator}>
        <SilencesPage />
      </As>,
    );
    const table = await screen.findByRole("table", { name: "Silences" });
    expect(within(table).getAllByText("web-1")).toHaveLength(2);
    expect(within(table).getByText("everything")).toBeInTheDocument();
    expect(within(table).getByText("Upcoming")).toBeInTheDocument();
    expect(within(table).getAllByText("Active")).toHaveLength(2);
    expect(within(table).getByText("A removed user")).toBeInTheDocument();
    // can_end decides who sees the button.
    expect(within(table).getAllByRole("button", { name: /End the silence/ })).toHaveLength(2);
  });

  it("says when nothing is silenced", async () => {
    stubFetch({ "GET /api/silences": { status: 200, body: { silences: [] } } });
    render(
      <As user={viewer}>
        <SilencesPage />
      </As>,
    );
    expect(await screen.findByText(/Nothing is silenced/)).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "New silence" })).not.toBeInTheDocument();
  });

  it("offers a customer administrator its customer, not everything", async () => {
    const fetchMock = stubFetch({
      "GET /api/silences": { status: 200, body: { silences: [] } },
      ...choices,
      "POST /api/silences": { status: 201, body: silence() },
    });
    render(
      <As user={customerAdmin}>
        <SilencesPage />
      </As>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "New silence" }));
    const what = await screen.findByRole("combobox", { name: "What" });
    await screen.findByRole("option", { name: "Customer acme" });
    expect(what).toHaveValue("");
    expect(within(what).queryByRole("option", { name: "Everything" })).not.toBeInTheDocument();
    expect(within(what).queryByRole("option", { name: "Customer globex" })).not.toBeInTheDocument();
    await userEvent.selectOptions(what, "Site paris");
    await userEvent.type(screen.getByLabelText(/^Until/), "2030-01-02T12:00");
    await userEvent.click(screen.getByRole("button", { name: "Silence" }));
    const body = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "POST"));
    expect(body).toMatchObject({ scope: "site", target_id: "site-1" });
  });
});
