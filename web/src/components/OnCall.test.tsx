import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { setActiveLocale } from "@/i18n/locale";
import type { Contact, OnCallSchedule, OnCallTurn } from "@/lib/api";
import { formatWhen } from "@/lib/onCall";
import { As, customerAdmin, me } from "@/test/admin";
import { stubFetch } from "@/test/fetch";

import { OnCallPage } from "./OnCall";

vi.mock("next/navigation", () => import("@/test/navigation"));

const HOUR = 3_600_000;
const at = (hours: number) => new Date(Date.now() + hours * HOUR).toISOString();

function turn(username: string | null, from: number, to: number, overrides: Partial<OnCallTurn> = {}): OnCallTurn {
  return { seat: 0, starts_at: at(from), ends_at: at(to), user_id: username && `id-${username}`, username, override_id: null, problem: null, ...overrides };
}

const NOW_TURN = turn("alice", -24, 144);
const NEXT_TURN = turn("bob", 144, 312);

function schedule(overrides: Partial<OnCallSchedule> = {}): OnCallSchedule {
  const turns = [NOW_TURN, NEXT_TURN];
  return {
    id: "s1",
    name: "Ops",
    customer_id: "c1",
    customer_name: "acme",
    site_id: null,
    site_name: null,
    time_zone: "Europe/Paris",
    rotation: "weekly",
    handoff_time: "09:00",
    handoff_day: 1,
    members: [
      { user_id: "id-alice", username: "alice", problem: null },
      { user_id: "id-bob", username: "bob", problem: null },
    ],
    people_per_turn: 1,
    overrides: [],
    now: NOW_TURN,
    next: NEXT_TURN,
    now_all: [NOW_TURN],
    next_all: [NEXT_TURN],
    turns,
    fallback: ["boss"],
    mail_host: "smtp.example.com",
    can_manage: true,
    ...overrides,
  };
}

const contact: Contact = {
  email: "alice@example.com",
  kind: null,
  settings: null,
  has_secret: false,
  last_sent_at: null,
  last_error: null,
  last_error_at: null,
};
const noContact: Contact = { ...contact, email: null };
const bodyOf = (call: Parameters<typeof fetch> | undefined) => JSON.parse(String(call?.[1]?.body)) as Record<string, unknown>;
const people = {
  status: 200,
  body: {
    people: [
      { id: "id-alice", username: "alice", has_contact: true },
      { id: "id-bob", username: "bob", has_contact: true },
      { id: "id-carol", username: "carol", has_contact: false },
    ],
  },
};

describe("OnCallPage", () => {
  it("says who is on call now and next, in plain words", async () => {
    stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [schedule()] } },
      "GET /api/on-call/contact": { status: 200, body: contact },
    });
    render(
      <As user={customerAdmin}>
        <OnCallPage />
      </As>,
    );
    const card = await screen.findByRole("region", { name: "Ops" });
    expect(within(card).getByText("All of acme · Hands off every Monday at 09:00, Europe/Paris time")).toBeInTheDocument();
    expect(within(card).getByText("On call now").nextSibling).toHaveTextContent("alice");
    expect(within(card).getByText("Next").nextSibling).toHaveTextContent("bob");
    expect(within(card).getByText(`from ${formatWhen(new Date(NEXT_TURN.starts_at))}`)).toBeInTheDocument();
    const table = within(card).getByRole("table", { name: "Coming weeks of Ops" });
    expect(within(table).getAllByRole("row")).toHaveLength(3);
    expect(within(table).getByText("Now")).toBeInTheDocument();
    expect(within(card).queryByText(/nobody can be told/)).not.toBeInTheDocument();
    expect(screen.getByText("alice@example.com")).toBeInTheDocument();
  });

  it("speaks French when that is the language", async () => {
    setActiveLocale("fr");
    stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [schedule()] } },
      "GET /api/on-call/contact": { status: 200, body: contact },
    });
    render(
      <As user={customerAdmin}>
        <OnCallPage />
      </As>,
    );
    const card = await screen.findByRole("region", { name: "Ops" });
    expect(within(card).getByText("Tout acme · Relève chaque lundi à 09:00, heure de Europe/Paris")).toBeInTheDocument();
    expect(within(card).getByText("D’astreinte maintenant").nextSibling).toHaveTextContent("alice");
    const table = within(card).getByRole("table", { name: "Semaines à venir de Ops" });
    expect(within(table).getByText("Maintenant")).toBeInTheDocument();
    expect(screen.getByRole("heading", { name: "Astreinte" })).toBeInTheDocument();
    expect(screen.getByText("Comment vous joindre")).toBeInTheDocument();
  });

  it("shows gaps before they happen, and who is told instead", async () => {
    const gap = turn("bob", 144, 312, { problem: "no_contact" });
    stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [schedule({ turns: [turn("alice", -24, 144), gap], next: gap })] } },
      "GET /api/on-call/contact": { status: 200, body: contact },
    });
    render(
      <As user={customerAdmin}>
        <OnCallPage />
      </As>,
    );
    const banner = await screen.findByText(/nobody can be told in person/);
    expect(banner.parentElement).toHaveTextContent(
      "bob has not added an email address or a chat. During this gap, boss is told instead.",
    );
    expect(screen.getByText(/Gap: bob has not added/)).toBeInTheDocument();
  });

  it("puts someone else on call for a turn in a few clicks", async () => {
    const fetchMock = stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [schedule()] } },
      "GET /api/on-call/contact": { status: 200, body: contact },
      "GET /api/on-call/people?customer_id=c1": people,
      "POST /api/on-call/schedules/s1/overrides": { status: 201, body: schedule() },
    });
    render(
      <As user={customerAdmin}>
        <OnCallPage />
      </As>,
    );
    const next = NEXT_TURN;
    await userEvent.click(await screen.findByRole("button", { name: `Someone else covers bob from ${formatWhen(new Date(next.starts_at))}` }));
    const form = screen.getByRole("form", { name: "Someone else covers" });
    expect(within(form).getByText("Instead of bob.")).toBeInTheDocument();
    // Bob is not offered to cover himself.
    await screen.findByRole("option", { name: "carol" });
    expect(within(form).queryByRole("option", { name: "bob" })).not.toBeInTheDocument();
    await userEvent.selectOptions(within(form).getByLabelText(/Who covers/), "alice");
    await userEvent.click(within(form).getByRole("button", { name: "Cover" }));
    const body = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "POST"));
    expect(body.user_id).toBe("id-alice");
    // The turn's own period, to the minute.
    expect(Math.abs(Date.parse(String(body.starts_at)) - Date.parse(next.starts_at))).toBeLessThan(60_000);
    expect(Math.abs(Date.parse(String(body.ends_at)) - Date.parse(next.ends_at))).toBeLessThan(60_000);
  });

  it("creates a schedule with people in turn", async () => {
    const fetchMock = stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [] } },
      "GET /api/on-call/contact": { status: 200, body: contact },
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] } },
      "GET /api/sites": { status: 200, body: { sites: [{ id: "site-1", name: "paris", customer_id: "c1", customer_name: "acme", server_count: 1 }] } },
      "GET /api/on-call/people?customer_id=c1": people,
      "POST /api/on-call/schedules": { status: 201, body: schedule() },
    });
    render(
      <As user={customerAdmin}>
        <OnCallPage />
      </As>,
    );
    // The intro just above already says alerts go to the channels besides.
    expect(await screen.findByText(/^No schedule yet\.$/)).toBeInTheDocument();
    expect(screen.queryByText(/Times are shown in your time zone/)).toBeNull();
    await userEvent.click(screen.getByRole("button", { name: "New schedule" }));
    const form = screen.getByRole("form", { name: "New schedule" });
    // The legend and the Add someone field say it: no third sentence.
    expect(within(form).queryByText(/Nobody yet/)).toBeNull();
    expect(within(form).getByText("The first person is on call from now.")).toBeInTheDocument();
    await userEvent.selectOptions(within(form).getByLabelText(/On call for/), "All of acme");
    await screen.findByRole("option", { name: "bob" });
    await userEvent.selectOptions(within(form).getByLabelText("Add someone"), "bob");
    await userEvent.selectOptions(within(form).getByLabelText("Add someone"), "carol (no email or chat yet)");
    expect(within(form).getByText(/carol has not said how to reach them/)).toBeInTheDocument();
    await userEvent.click(within(form).getByRole("button", { name: "Move carol up" }));
    await userEvent.selectOptions(within(form).getByLabelText("On"), "Friday");
    await userEvent.click(within(form).getByRole("button", { name: "Create schedule" }));
    const body = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "POST"));
    expect(body).toMatchObject({
      customer_id: "c1",
      site_id: null,
      rotation: "weekly",
      handoff_day: 5,
      handoff_time: "09:00",
      members: ["id-carol", "id-bob"],
      people_per_turn: 1,
    });
    expect(typeof body.time_zone).toBe("string");
  });

  it("puts several people on call at once and covers one of them", async () => {
    const bob = turn("bob", -24, 144, { seat: 1 });
    const carol = turn("carol", 144, 312);
    const dan = turn("dan", 144, 312, { seat: 1 });
    const pair = schedule({
      people_per_turn: 2,
      members: ["alice", "bob", "carol", "dan"].map((n) => ({ user_id: `id-${n}`, username: n, problem: null })),
      now_all: [NOW_TURN, bob],
      next_all: [carol, dan],
      next: carol,
      turns: [NOW_TURN, bob, carol, dan],
    });
    const fetchMock = stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [pair] } },
      "GET /api/on-call/contact": { status: 200, body: contact },
      "GET /api/on-call/people?customer_id=c1": people,
      "POST /api/on-call/schedules/s1/overrides": { status: 201, body: pair },
      "PUT /api/on-call/schedules/s1": { status: 200, body: pair },
      "GET /api/customers": { status: 200, body: { customers: [{ id: "c1", name: "acme", site_count: 1 }] } },
      "GET /api/sites": { status: 200, body: { sites: [] } },
    });
    render(
      <As user={customerAdmin}>
        <OnCallPage />
      </As>,
    );
    const card = await screen.findByRole("region", { name: "Ops" });
    expect(within(card).getByText(/2 people at once/)).toBeInTheDocument();
    expect(within(card).getByText("On call now").nextSibling).toHaveTextContent("alice and bob");
    expect(within(card).getByText("Next").nextSibling).toHaveTextContent("carol and dan");

    // Covering without a turn asks whose place.
    await userEvent.click(within(card).getByRole("button", { name: "Someone else covers…" }));
    const form = screen.getByRole("form", { name: "Someone else covers" });
    await userEvent.selectOptions(within(form).getByLabelText("In the place of"), "bob");
    await screen.findByRole("option", { name: "carol" });
    await userEvent.selectOptions(within(form).getByLabelText(/Who covers/), "carol");
    await userEvent.click(within(form).getByRole("button", { name: "Cover" }));
    const cover = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "POST"));
    expect(cover).toMatchObject({ user_id: "id-carol", seat: 1 });

    // The form keeps the number and says how turns go.
    await userEvent.click(await within(card).findByRole("button", { name: "Edit" }));
    const edit = await screen.findByRole("form", { name: "Edit Ops" });
    expect(within(edit).getByLabelText(/^On call at once/)).toHaveValue("2");
    expect(within(edit).getByText(/alice and bob first, then the next 2/)).toBeInTheDocument();
    await userEvent.selectOptions(within(edit).getByLabelText(/^On call at once/), "3");
    await userEvent.click(within(edit).getByRole("button", { name: "Save" }));
    const saved = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "PUT"));
    expect(saved.people_per_turn).toBe(3);
  });

  it("lets a member look without managing, and asks how to reach them", async () => {
    const fetchMock = stubFetch({
      "GET /api/on-call/schedules": { status: 200, body: { schedules: [schedule({ can_manage: false, fallback: [], mail_host: null })] } },
      "GET /api/on-call/contact": { status: 200, body: noContact },
      "PUT /api/on-call/contact": { status: 200, body: contact },
    });
    render(
      <As user={me({ role: "operator", customer_id: "c1", site_id: null })}>
        <OnCallPage />
      </As>,
    );
    const card = await screen.findByRole("region", { name: "Ops" });
    expect(within(card).queryByRole("button", { name: /Cover|Edit|Delete/ })).not.toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "New schedule" })).not.toBeInTheDocument();
    expect(screen.getByText("When you are on call, alerts come to you here.")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Email"), "alice@example.com");
    // Not "Email only" right under the Email field.
    expect(within(screen.getByLabelText("Also by")).getByRole("option", { name: "Nothing else" })).toBeInTheDocument();
    await userEvent.selectOptions(screen.getByLabelText("Also by"), "Slack");
    await userEvent.type(screen.getByLabelText(/Slack webhook URL/), "https://hooks.slack.com/services/T/B/X");
    await userEvent.click(screen.getByRole("button", { name: "Save" }));
    const body = bodyOf(fetchMock.mock.calls.find((c) => c[1]?.method === "PUT"));
    expect(body).toEqual({
      email: "alice@example.com",
      kind: "slack",
      settings: {},
      secret: "https://hooks.slack.com/services/T/B/X",
    });
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
  });
});
