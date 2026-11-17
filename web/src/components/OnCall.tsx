"use client";

import { type FormEvent, useCallback, useState } from "react";

import { rich, t } from "@/i18n";
import {
  type Contact,
  type ContactRequest,
  type OnCallPerson,
  type OnCallSchedule,
  type OnCallScheduleRequest,
  type OnCallTurn,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { isAdministrator, multiCustomer, useMe, useMultiCustomer } from "@/lib/me";
import { channelScopes, scopeValue } from "@/lib/notifications";
import {
  WEEKDAYS,
  browserTimeZone,
  formatWhen,
  gaps,
  handoffRule,
  move,
  placeName,
  problemText,
  timeZones,
  toLocalInput,
  weekdayName,
  who,
} from "@/lib/onCall";
import { useScope } from "@/lib/scope";
import { fromLocalInput } from "@/lib/silences";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import { FollowUpSection } from "./FollowUp";
import { CHAT, chatHelp } from "./NotificationsAdmin";
import styles from "./OnCall.module.css";

type Also = NonNullable<Contact["kind"]> | "";

/** How the signed-in user is reached when on call: their own, nobody else's. */
function ContactSection() {
  const { data, error, refresh } = usePolledData(useCallback((signal: AbortSignal) => api.contact(signal), []));
  const [editing, setEditing] = useState(false);
  const [email, setEmail] = useState("");
  const [also, setAlso] = useState<Also>("");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: "ok" | "bad"; text: string } | null>(null);

  function edit(contact: Contact) {
    setEmail(contact.email ?? "");
    setAlso(contact.kind ?? "");
    setUrl(contact.kind === "webhook" ? (contact.settings?.url ?? "") : "");
    setMessage(null);
    setEditing(true);
  }

  const savedChat = data?.kind && data.kind !== "webhook" && data.kind === also && data.has_secret;
  const ways = data
    ? [
        data.email,
        data.kind &&
          (data.kind === "webhook" ? t("onCall.contact.webhookTo", { host: hostOf(data.settings?.url) }) : CHAT[data.kind].name),
      ].filter((way): way is string => Boolean(way))
    : [];
  const described = ways.length === 2 ? t("onCall.contact.both", { first: ways[0]!, second: ways[1]! }) : (ways[0] ?? "");

  async function save(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setMessage(null);
    const body: ContactRequest = {
      email: email.trim() || null,
      kind: also || null,
      settings: also === "webhook" ? { url } : also ? {} : null,
      secret: also && also !== "webhook" ? url || null : null,
    };
    try {
      await api.setContact(body);
      setEditing(false);
      setUrl("");
      setMessage({ tone: "ok", text: t("onCall.contact.saved") });
      refresh();
    } catch (e) {
      setMessage({ tone: "bad", text: describeError(e) });
    } finally {
      setBusy(false);
    }
  }

  async function test() {
    setBusy(true);
    setMessage(null);
    try {
      const { results } = await api.testContact();
      const failed = results.filter((r) => !r.ok);
      setMessage(
        failed.length === 0
          ? { tone: "ok", text: t("onCall.contact.testSent") }
          : { tone: "bad", text: failed.map((r) => r.error).join(" ") },
      );
      refresh();
    } catch (e) {
      setMessage({ tone: "bad", text: describeError(e) });
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className={admin.section} aria-labelledby="contact-title">
      <div className={admin.sectionHead}>
        <span id="contact-title" className={admin.sectionTitle}>
          {t("onCall.contact.title")}
        </span>
        <span className={admin.dim}>{t("onCall.contact.intro")}</span>
      </div>
      <div className={styles.body}>
        {!data && error !== null ? (
          <p role="alert">{describeError(error)}</p>
        ) : editing || (data && !data.email && !data.kind) ? (
          <form className={styles.form} onSubmit={(e) => void save(e)}>
            <label className={admin.field}>
              {t("onCall.contact.email")}
              <input
                className={fieldClass}
                type="email"
                autoComplete="email"
                value={email}
                placeholder="you@example.com"
                onChange={(e) => setEmail(e.target.value)}
              />
            </label>
            <label className={admin.field}>
              {t("onCall.contact.also")}
              <select className={fieldClass} value={also} onChange={(e) => setAlso(e.target.value as Also)}>
                <option value="">{t("onCall.contact.emailOnly")}</option>
                {(Object.keys(CHAT) as (keyof typeof CHAT)[]).map((kind) => (
                  <option key={kind} value={kind}>
                    {CHAT[kind].name}
                  </option>
                ))}
                <option value="webhook">{t("onCall.contact.webhook")}</option>
              </select>
            </label>
            {also && (
              <label className={admin.field}>
                {also === "webhook" ? t("onCall.contact.url") : t("onCall.contact.chatUrl", { service: CHAT[also].name })}
                <input
                  className={fieldClass}
                  type={also === "webhook" ? "url" : "password"}
                  autoComplete="off"
                  value={url}
                  placeholder={savedChat ? t("onCall.contact.savedUrl") : also === "webhook" ? "https://…" : CHAT[also].placeholder}
                  onChange={(e) => setUrl(e.target.value)}
                />
                <span className={admin.fieldNote}>
                  {also === "webhook"
                    ? t("onCall.contact.webhookNote")
                    : t("onCall.contact.chatNote", { help: chatHelp(also) })}
                </span>
              </label>
            )}
            <div className={admin.formFoot}>
              <Button type="submit" variant="k" disabled={busy}>
                {busy ? t("onCall.saving") : t("onCall.save")}
              </Button>
              {editing && (
                <Button onClick={() => setEditing(false)} disabled={busy}>
                  {t("onCall.cancel")}
                </Button>
              )}
            </div>
          </form>
        ) : data ? (
          <div className={styles.contactLine}>
            <span>{rich("onCall.contact.by", { how: <strong>{described}</strong> })}</span>
            {data.last_error && <Label tone="bad">{data.last_error}</Label>}
            <Button size="small" onClick={() => edit(data)}>
              {t("onCall.contact.change")}
            </Button>
            <Button size="small" disabled={busy} onClick={() => void test()}>
              {t("onCall.contact.sendTest")}
            </Button>
          </div>
        ) : null}
        {message && (
          <p role={message.tone === "bad" ? "alert" : "status"} className={message.tone === "bad" ? styles.bad : styles.ok}>
            {message.text}
          </p>
        )}
      </div>
    </section>
  );
}

function hostOf(url: string | undefined): string {
  try {
    return new URL(url ?? "").host;
  } catch {
    return url ?? "";
  }
}

/** "Someone else covers": a person and a period, prefilled with a turn. */
function CoverForm({
  schedule,
  turn,
  onDone,
  onCancel,
}: {
  schedule: OnCallSchedule;
  turn: OnCallTurn | null;
  onDone: () => void;
  onCancel: () => void;
}) {
  const { data } = usePolledData(
    useCallback(
      (signal: AbortSignal) => api.onCallPeople({ customer_id: schedule.site_id ? null : schedule.customer_id, site_id: schedule.site_id }, signal),
      [schedule.customer_id, schedule.site_id],
    ),
  );
  const now = new Date();
  const [user, setUser] = useState("");
  const [from, setFrom] = useState(toLocalInput(turn ? new Date(Math.max(Date.parse(turn.starts_at), now.getTime())) : now));
  const [until, setUntil] = useState(toLocalInput(new Date(turn ? turn.ends_at : now.getTime() + 86_400_000)));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const members = schedule.members.filter((m) => m.user_id && m.username) as { user_id: string; username: string }[];
  const people: { id: string; username: string }[] = [
    ...members.map((m) => ({ id: m.user_id, username: m.username })),
    ...(data?.people ?? []).filter((p) => !members.some((m) => m.user_id === p.id)),
  ].filter((p) => p.id !== turn?.user_id);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const start = fromLocalInput(from);
    const end = fromLocalInput(until);
    if (!user || !end) return setError(t("onCall.cover.missing"));
    setBusy(true);
    setError(null);
    try {
      await api.addOnCallOverride(schedule.id, { user_id: user, starts_at: start?.toISOString(), ends_at: end.toISOString() });
      onDone();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} onSubmit={(e) => void submit(e)} aria-label={t("onCall.cover.form")}>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("onCall.cover.who")}
          <select className={fieldClass} required value={user} onChange={(e) => setUser(e.target.value)}>
            <option value="" disabled>
              {t("onCall.choose")}
            </option>
            {people.map((p) => (
              <option key={p.id} value={p.id}>
                {p.username}
              </option>
            ))}
          </select>
          {turn?.username && <span className={admin.fieldNote}>{t("onCall.cover.insteadOf", { name: turn.username })}</span>}
        </label>
        <label className={admin.field}>
          {t("onCall.from")}
          <input className={fieldClass} type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
        </label>
        <label className={admin.field}>
          {t("onCall.until")}
          <input className={fieldClass} type="datetime-local" required value={until} onChange={(e) => setUntil(e.target.value)} />
        </label>
      </div>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("onCall.saving") : t("onCall.cover.submit")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("onCall.cancel")}
        </Button>
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

async function loadPlaces(signal: AbortSignal) {
  const [{ customers }, { sites }] = await Promise.all([api.customers(signal), api.sites(signal)]);
  return { customers, sites };
}

/** Creating or editing a schedule: what it covers, who, in which order, and when hand-offs happen. */
function ScheduleForm({
  schedule,
  onSaved,
  onCancel,
}: {
  schedule?: OnCallSchedule;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const me = useMe();
  const { data: places } = usePolledData(loadPlaces);
  const scopes = channelScopes(me, places?.customers ?? [], places?.sites ?? [], schedule);
  const [name, setName] = useState(schedule?.name ?? "");
  const [scope, setScope] = useState(schedule ? scopeValue(schedule) : "");
  const chosen = scopes.find((s) => s.value === scope) ?? (scopes.length === 1 ? scopes[0] : undefined);
  const [members, setMembers] = useState<{ id: string; username: string }[]>(
    (schedule?.members ?? []).flatMap((m) => (m.user_id && m.username ? [{ id: m.user_id, username: m.username }] : [])),
  );
  const [rotation, setRotation] = useState<OnCallScheduleRequest["rotation"]>(schedule?.rotation ?? "weekly");
  const [day, setDay] = useState(schedule?.handoff_day ?? 1);
  const [time, setTime] = useState(schedule?.handoff_time ?? "09:00");
  const [zone, setZone] = useState(schedule?.time_zone ?? browserTimeZone());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const peopleFetcher = useCallback(
    (signal: AbortSignal): Promise<{ people: OnCallPerson[] }> =>
      chosen ? api.onCallPeople({ customer_id: chosen.customer_id, site_id: chosen.site_id }, signal) : Promise.resolve({ people: [] }),
    // Only a change of place asks again.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [chosen?.value],
  );
  const { data: candidates } = usePolledData(peopleFetcher);
  const addable = (candidates?.people ?? []).filter((p) => !members.some((m) => m.id === p.id));

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!chosen) return setError(t("onCall.form.missingPlace"));
    setBusy(true);
    setError(null);
    const body: OnCallScheduleRequest = {
      name: name.trim() || chosen.place,
      customer_id: chosen.customer_id,
      site_id: chosen.site_id,
      time_zone: zone,
      rotation,
      handoff_time: time,
      handoff_day: rotation === "weekly" ? day : null,
      members: members.map((m) => m.id),
    };
    try {
      if (schedule) await api.updateOnCallSchedule(schedule.id, body);
      else await api.createOnCallSchedule(body);
      onSaved();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  const missing = (candidates?.people ?? []).filter((p) => members.some((m) => m.id === p.id) && !p.has_contact);
  return (
    <form className={styles.form} onSubmit={(e) => void submit(e)} aria-label={schedule ? t("onCall.form.edit", { name: schedule.name }) : t("onCall.form.new")}>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("onCall.form.name")}
          <input
            className={fieldClass}
            maxLength={80}
            value={name}
            placeholder={chosen ? chosen.place : t("onCall.form.namePlaceholder")}
            onChange={(e) => setName(e.target.value)}
          />
        </label>
        {scopes.length > 1 && (
          <label className={admin.field}>
            {t("onCall.form.for")}
            <select className={fieldClass} required value={chosen?.value ?? ""} onChange={(e) => setScope(e.target.value)}>
              <option value="" disabled>
                {t("onCall.choose")}
              </option>
              {scopes.map((s) => (
                <option key={s.value} value={s.value}>
                  {s.label}
                </option>
              ))}
            </select>
            {chosen && <span className={admin.fieldNote}>{t("onCall.form.forNote", { place: chosen.place })}</span>}
          </label>
        )}
      </div>
      <fieldset className={styles.people}>
        <legend>{t("onCall.form.people")}</legend>
        <ol className={styles.order}>
          {members.map((m, i) => (
            <li key={m.id}>
              <span>{m.username}</span>
              <Button size="small" disabled={i === 0} aria-label={t("onCall.form.up", { name: m.username })} onClick={() => setMembers(move(members, i, -1))}>
                ↑
              </Button>
              <Button size="small" disabled={i === members.length - 1} aria-label={t("onCall.form.down", { name: m.username })} onClick={() => setMembers(move(members, i, 1))}>
                ↓
              </Button>
              <Button size="small" aria-label={t("onCall.form.takeOff", { name: m.username })} onClick={() => setMembers(members.filter((x) => x.id !== m.id))}>
                {t("onCall.remove")}
              </Button>
            </li>
          ))}
        </ol>
        {chosen && (
          <label className={admin.field}>
            {t("onCall.form.add")}
            <select
              className={fieldClass}
              value=""
              onChange={(e) => {
                const person = addable.find((p) => p.id === e.target.value);
                if (person) setMembers([...members, { id: person.id, username: person.username }]);
              }}
            >
              <option value="">{addable.length ? t("onCall.choose") : t("onCall.form.nobodyElse")}</option>
              {addable.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.has_contact ? p.username : t("onCall.noContactYet", { name: p.username })}
                </option>
              ))}
            </select>
          </label>
        )}
        {missing.length > 0 && (
          <p className={admin.fieldNote}>
            {t("onCall.form.missingContact", { count: missing.length, names: missing.map((m) => m.username).join(", ") })}
          </p>
        )}
      </fieldset>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("onCall.form.handoff")}
          <select className={fieldClass} value={rotation} onChange={(e) => setRotation(e.target.value as typeof rotation)}>
            <option value="weekly">{t("onCall.form.weekly")}</option>
            <option value="daily">{t("onCall.form.daily")}</option>
          </select>
        </label>
        {rotation === "weekly" && (
          <label className={admin.field}>
            {t("onCall.form.on")}
            <select className={fieldClass} value={day} onChange={(e) => setDay(Number(e.target.value))}>
              {WEEKDAYS.map((weekday) => (
                <option key={weekday} value={weekday}>
                  {weekdayName(weekday)}
                </option>
              ))}
            </select>
          </label>
        )}
        <label className={admin.field}>
          {t("onCall.form.at")}
          <input className={fieldClass} type="time" required value={time} onChange={(e) => setTime(e.target.value)} />
        </label>
        <label className={admin.field}>
          {t("onCall.form.zone")}
          <select className={fieldClass} value={zone} onChange={(e) => setZone(e.target.value)}>
            {timeZones(zone).map((z) => (
              <option key={z} value={z}>
                {z.replaceAll("_", " ")}
              </option>
            ))}
          </select>
          <span className={admin.fieldNote}>{t("onCall.form.zoneNote")}</span>
        </label>
      </div>
      <p className={admin.fieldNote}>
        {schedule ? t("onCall.form.staysOn") : t("onCall.form.firstNow")}
      </p>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy || members.length === 0}>
          {busy ? t("onCall.saving") : schedule ? t("onCall.save") : t("onCall.form.create")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("onCall.cancel")}
        </Button>
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

/** One schedule: now and next, gaps ahead, the coming weeks, overrides. */
function ScheduleCard({ schedule, several, onChanged }: { schedule: OnCallSchedule; several: boolean; onChanged: () => void }) {
  const [covering, setCovering] = useState<OnCallTurn | null | undefined>(undefined);
  const [editing, setEditing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const place = placeName(schedule, several, useMultiCustomer());
  const { now, next } = schedule;
  const ahead = gaps(schedule);
  const isNow = (turn: OnCallTurn) => turn.starts_at === now?.starts_at;
  const why = (turn: OnCallTurn) => (turn.problem ? problemText(turn.problem, turn.username, place) : "");
  const fallback = schedule.fallback.length
    ? t("onCall.fallback.told", { count: schedule.fallback.length, names: schedule.fallback.join(", ") })
    : schedule.can_manage
      ? t("onCall.fallback.channels")
      : t("onCall.fallback.administrators");

  async function removeOverride(id: string) {
    setError(null);
    try {
      await api.deleteOnCallOverride(id);
      onChanged();
    } catch (e) {
      setError(describeError(e));
    }
  }

  if (editing) {
    return (
      <section className={admin.section} aria-label={schedule.name}>
        <div className={styles.body}>
          <ScheduleForm
            schedule={schedule}
            onCancel={() => setEditing(false)}
            onSaved={() => {
              setEditing(false);
              onChanged();
            }}
          />
        </div>
      </section>
    );
  }
  return (
    <section className={admin.section} aria-label={schedule.name}>
      <div className={admin.sectionHead}>
        <span className={admin.sectionTitle}>{schedule.name}</span>
        <span className={admin.dim}>
          {place} · {handoffRule(schedule)}
        </span>
      </div>
      <div className={styles.body}>
        <div className={styles.nowNext}>
          <div>
            <span className={styles.label}>{t("onCall.card.now")}</span>
            <strong className={now?.problem ? styles.bad : undefined}>{now ? who(now) : t("onCall.nobodyStart")}</strong>
            {now && <span className={admin.dim}>{t("onCall.card.until", { when: formatWhen(new Date(now.ends_at)) })}</span>}
          </div>
          {next && (
            <div>
              <span className={styles.label}>{t("onCall.card.next")}</span>
              <strong className={next.problem ? styles.bad : undefined}>{who(next)}</strong>
              <span className={admin.dim}>{t("onCall.card.from", { when: formatWhen(new Date(next.starts_at)) })}</span>
            </div>
          )}
        </div>
        {ahead.map((gap) => (
          <Banner key={gap.starts_at} tone={isNow(gap) ? "bad" : "warn"}>
            <strong>
              {isNow(gap)
                ? t("onCall.gap.now")
                : t("onCall.gap.period", { from: formatWhen(new Date(gap.starts_at)), until: formatWhen(new Date(gap.ends_at)) })}
            </strong>{" "}
            {t("onCall.gap.during", { why: why(gap), fallback })}
          </Banner>
        ))}
        {schedule.can_manage && !schedule.mail_host && schedule.members.some((m) => m.problem === "no_mail_server") && (
          <Banner tone="info">{t("onCall.card.mailHost")}</Banner>
        )}
        {covering !== undefined && (
          <CoverForm
            schedule={schedule}
            turn={covering}
            onCancel={() => setCovering(undefined)}
            onDone={() => {
              setCovering(undefined);
              onChanged();
            }}
          />
        )}
        <table className={styles.turns} aria-label={t("onCall.turns.label", { name: schedule.name })}>
          <thead>
            <tr>
              <th scope="col">{t("onCall.from")}</th>
              <th scope="col">{t("onCall.until")}</th>
              <th scope="col">{t("onCall.turns.onCall")}</th>
              <th scope="col">
                <span className="visually-hidden">{t("onCall.turns.notes")}</span>
              </th>
              {schedule.can_manage && (
                <th scope="col">
                  <span className="visually-hidden">{t("onCall.turns.actions")}</span>
                </th>
              )}
            </tr>
          </thead>
          <tbody>
            {schedule.turns.map((turn) => (
              <tr key={turn.starts_at}>
                <td>{isNow(turn) ? t("onCall.now") : formatWhen(new Date(turn.starts_at))}</td>
                <td>{formatWhen(new Date(turn.ends_at))}</td>
                <td>{who(turn)}</td>
                <td className={admin.wrap}>
                  {turn.problem ? (
                    <Label tone="warn">{t("onCall.gap", { why: why(turn) })}</Label>
                  ) : turn.override_id ? (
                    <Label tone="info">{t("onCall.covering")}</Label>
                  ) : null}
                </td>
                {schedule.can_manage && (
                  <td>
                    <Button
                      size="small"
                      aria-label={t("onCall.cover.someoneFor", { who: who(turn), when: formatWhen(new Date(turn.starts_at)) })}
                      onClick={() => setCovering(turn)}
                    >
                      {t("onCall.cover.button")}
                    </Button>
                  </td>
                )}
              </tr>
            ))}
          </tbody>
        </table>
        {schedule.overrides.length > 0 && (
          <ul className={styles.overrides} aria-label={t("onCall.covering")}>
            {schedule.overrides.map((o) => (
              <li key={o.id}>
                {t("onCall.cover.line", {
                  name: o.username ?? t("onCall.removedUserStart"),
                  from: formatWhen(new Date(o.starts_at)),
                  until: formatWhen(new Date(o.ends_at)),
                })}{" "}
                {schedule.can_manage && (
                  <Button
                    size="small"
                    aria-label={
                      o.username
                        ? t("onCall.cover.remove", { name: o.username, when: formatWhen(new Date(o.starts_at)) })
                        : t("onCall.cover.removeUnnamed", { when: formatWhen(new Date(o.starts_at)) })
                    }
                    onClick={() => void removeOverride(o.id)}
                  >
                    {t("onCall.remove")}
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
        {error && <p role="alert">{error}</p>}
        {schedule.can_manage && (
          <div className={styles.actions}>
            <Button onClick={() => setCovering(null)}>{t("onCall.cover.someone")}</Button>
            <Button onClick={() => setEditing(true)}>{t("onCall.edit")}</Button>
            <InlineConfirm
              trigger={t("onCall.delete.trigger")}
              title={t("onCall.delete.title", { name: schedule.name })}
              consequence={t("onCall.delete.consequence")}
              confirmLabel={t("onCall.delete.confirm", { name: schedule.name })}
              onConfirm={async () => {
                await api.deleteOnCallSchedule(schedule.id);
                onChanged();
              }}
              describeError={describeError}
            />
          </div>
        )}
      </div>
    </section>
  );
}

const load = (signal: AbortSignal) => api.onCallSchedules(signal);

/** `/on-call/`: who is on call now and next, the coming weeks, and how the user is reached. */
export function OnCallPage() {
  const me = useMe();
  const scope = useScope();
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);
  const [adding, setAdding] = useState(false);
  const schedules = (data?.schedules ?? []).filter(
    (s) =>
      (!scope.customerId || !s.customer_id || s.customer_id === scope.customerId) &&
      (!scope.siteId || !s.site_id || s.site_id === scope.siteId),
  );
  const several = multiCustomer(me) && new Set((data?.schedules ?? []).map((s) => s.customer_id).filter(Boolean)).size > 1;
  const canCreate = isAdministrator(me);
  return (
    <Page>
      <PageHeader
        title={t("onCall.title")}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={
          canCreate && (
            <Button variant="k" aria-expanded={adding} onClick={() => setAdding(!adding)}>
              {t("onCall.form.new")}
            </Button>
          )
        }
      />
      <p className={styles.intro}>{t("onCall.intro")}</p>
      <ContactSection />
      {adding && (
        <section className={admin.section} aria-label={t("onCall.form.new")}>
          <div className={styles.body}>
            <ScheduleForm
              onCancel={() => setAdding(false)}
              onSaved={() => {
                setAdding(false);
                refresh();
              }}
            />
          </div>
        </section>
      )}
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("onCall.what")} error={describeError(error)} onRetry={refresh} />
      ) : data && schedules.length === 0 ? (
        !adding && (
        <EmptyState>
          {canCreate ? t("onCall.empty.admin") : t("onCall.empty.other")}
          </EmptyState>
        )
      ) : (
        schedules.map((s) => <ScheduleCard key={s.id} schedule={s} several={several} onChanged={refresh} />)
      )}
      <FollowUpSection enabled={canCreate} />
    </Page>
  );
}
