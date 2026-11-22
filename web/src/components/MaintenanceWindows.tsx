"use client";

import { type FormEvent, type ReactNode, useCallback, useState } from "react";

import { t } from "@/i18n";
import {
  type EffectiveSensor,
  type MaintenanceRepeat,
  type MaintenanceWindow,
  type MaintenanceWindowRequest,
  type WeekdayName,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { MAX_MINUTES, WEEKDAY_NAMES, checkName, dayName, describeNext, describeRule, describeWhat, minutesOf, monthName } from "@/lib/maintenance";
import { modifiesSite, useMe } from "@/lib/me";
import { browserTimeZone, timeZones } from "@/lib/onCall";
import { useScope } from "@/lib/scope";
import { defaultReason } from "@/lib/silences";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { RowMenu } from "@/components/ui/RowMenu";
import { Banner, EmptyState, ErrorState } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatDay } from "@/components/ui/time";
import { Switch } from "@/components/ui/Switch";
import { SegmentedControl } from "@/components/ui/SegmentedControl";

import admin from "./AdminPages.module.css";
import styles from "./Silences.module.css";

/** Where a form starts: a site, and the servers of a server page. */
export interface WindowPlace {
  siteId: string;
  serverIds: string[];
}

const pad = (n: number) => String(n).padStart(2, "0");
const clock = (minutes: number) => `${pad(Math.floor(minutes / 60) % 24)}:${pad(minutes % 60)}`;
const today = () => {
  const d = new Date();
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
};
const MONTHS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

/** What the form holds, as people choose it: a start and end time and the day it ends. */
interface Draft {
  siteId: string;
  wholeSite: boolean;
  serverIds: string[];
  checks: string[];
  repeat: MaintenanceRepeat;
  every: number;
  weekdays: WeekdayName[];
  byWeek: boolean;
  monthDay: number;
  monthWeek: number;
  months: number[];
  from: string;
  to: string;
  /** Days after the start day it ends on. */
  endsAfter: number;
  zone: string;
  startsOn: string;
  endsOn: string;
  reason: string;
}

function draftOf(window: MaintenanceWindow | null, place: WindowPlace | null): Draft {
  if (!window) {
    return {
      siteId: place?.siteId ?? "",
      wholeSite: !place?.serverIds.length,
      serverIds: place?.serverIds ?? [],
      checks: [],
      repeat: "weekly",
      every: 1,
      weekdays: ["sunday"],
      byWeek: false,
      monthDay: 1,
      monthWeek: 1,
      months: [],
      from: "02:00",
      to: "04:00",
      endsAfter: 0,
      zone: browserTimeZone(),
      startsOn: today(),
      endsOn: "",
      reason: defaultReason(),
    };
  }
  const start = minutesOf(window.start_time);
  return {
    siteId: window.site_id,
    wholeSite: window.whole_site ?? window.servers.length === 0,
    serverIds: window.servers.map((s) => s.id),
    checks: (window.checks ?? []).map((c) => `${c.name}\t${c.instance}`),
    repeat: window.repeat,
    every: window.every,
    weekdays: window.weekdays,
    byWeek: window.month_week != null,
    monthDay: window.month_day ?? 1,
    monthWeek: window.month_week ?? 1,
    months: window.months.length === 12 ? [] : window.months,
    from: window.start_time,
    to: clock(start + window.duration_minutes),
    endsAfter: Math.floor((start + window.duration_minutes - 1) / (24 * 60)),
    zone: window.time_zone,
    startsOn: window.starts_on,
    endsOn: window.ends_on ?? "",
    reason: window.reason,
  };
}

/** The draft as the API takes it, or why not. */
export function requestOf(draft: Draft): MaintenanceWindowRequest | string {
  const start = minutesOf(draft.from);
  const end = minutesOf(draft.to) + draft.endsAfter * 24 * 60;
  const duration = end - start;
  if (duration === 0) return t("maintenance.form.noLength");
  if (duration < 0 || duration > MAX_MINUTES) return duration < 0 ? t("maintenance.form.noLength") : t("maintenance.form.tooLong");
  if (!draft.wholeSite && draft.serverIds.length === 0) return t("maintenance.form.noServers");
  if (draft.repeat === "weekly" && draft.weekdays.length === 0) return t("maintenance.form.noDay");
  const monthly = draft.repeat === "monthly";
  return {
    site_id: draft.siteId,
    server_ids: draft.wholeSite ? [] : draft.serverIds,
    checks: draft.checks.length
      ? draft.checks.map((key) => {
          const [name = "", instance = ""] = key.split("\t");
          return { name, instance };
        })
      : null,
    repeat: draft.repeat,
    every: monthly ? 1 : draft.every,
    weekdays: monthly && !draft.byWeek ? [] : monthly ? draft.weekdays.slice(0, 1) : draft.repeat === "weekly" ? draft.weekdays : [],
    month_day: monthly && !draft.byWeek ? draft.monthDay : null,
    month_week: monthly && draft.byWeek ? draft.monthWeek : null,
    months: monthly ? draft.months : [],
    start_time: draft.from,
    duration_minutes: duration,
    time_zone: draft.zone,
    starts_on: draft.startsOn || null,
    ends_on: draft.endsOn || null,
    reason: draft.reason,
  };
}

async function loadPlaces(signal: AbortSignal) {
  const [{ sites }, { servers }] = await Promise.all([api.sites(signal), api.servers(signal)]);
  return { sites, servers };
}

/** A window's place, rule and reason; saving creates it, or replaces `window`. */
export function WindowForm({
  window,
  place,
  onSaved,
  onCancel,
}: {
  window: MaintenanceWindow | null;
  place: WindowPlace | null;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const me = useMe();
  const [draft, setDraft] = useState(() => draftOf(window, place));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { data: places } = usePolledData(loadPlaces);
  const oneServer = !draft.wholeSite && draft.serverIds.length === 1 ? draft.serverIds[0] : null;
  const checksOf = useCallback(
    async (signal: AbortSignal): Promise<EffectiveSensor[]> => {
      if (oneServer) return (await api.serverSensors(oneServer, signal)).sensors;
      return draft.siteId ? (await api.siteSensors(draft.siteId, signal)).sensors : [];
    },
    [oneServer, draft.siteId],
  );
  const { data: sensors } = usePolledData(checksOf);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }));
  const toggle = <T,>(items: T[], item: T) => (items.includes(item) ? items.filter((i) => i !== item) : [...items, item]);
  const request = requestOf(draft);

  const sites = (places?.sites ?? []).filter((s) => modifiesSite(me, s.customer_id, s.id));
  const servers = (places?.servers ?? []).filter((s) => s.site_id === draft.siteId);
  const zones = timeZones(draft.zone);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (typeof request === "string") return setError(request);
    setBusy(true);
    setError(null);
    try {
      if (window) await api.updateMaintenanceWindow(window.id, request);
      else await api.createMaintenanceWindow(request);
      onSaved();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} aria-label={window ? t("maintenance.change.label", { reason: window.reason }) : t("maintenance.add")} onSubmit={(e) => void submit(e)}>
      <label className={admin.field}>
        {t("maintenance.form.site")}
        <select className={fieldClass} required value={draft.siteId} onChange={(e) => setDraft((d) => ({ ...d, siteId: e.target.value, serverIds: [], checks: [] }))}>
          <option value="" disabled>
            {t("maintenance.form.chooseSite")}
          </option>
          {sites.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
          {draft.siteId && !sites.some((s) => s.id === draft.siteId) && <option value={draft.siteId}>{window?.site_name ?? draft.siteId}</option>}
        </select>
      </label>
      <fieldset className={admin.fieldset}>
        <legend className={admin.legend}>{t("maintenance.form.cover")}</legend>
        <SegmentedControl
          label={t("maintenance.form.cover")}
          options={[
            { value: "site", label: t("maintenance.form.wholeSite") },
            { value: "servers", label: t("maintenance.form.someServers") },
          ]}
          value={draft.wholeSite ? "site" : "servers"}
          onChange={(v) => set("wholeSite", v === "site")}
        />
        {!draft.wholeSite && (
          <div className={styles.choices} role="group" aria-label={t("maintenance.form.servers")}>
            {servers.map((s) => (
              <label key={s.id}>
                <input type="checkbox" checked={draft.serverIds.includes(s.id)} onChange={() => setDraft((d) => ({ ...d, serverIds: toggle(d.serverIds, s.id), checks: [] }))} />
                {s.hostname}
              </label>
            ))}
          </div>
        )}
      </fieldset>
      {(sensors?.length ?? 0) > 0 && (
        <fieldset className={admin.fieldset}>
          <legend className={admin.legend}>{t("maintenance.form.checks")}</legend>
          <div className={styles.choices}>
            {sensors!.map((s) => {
              const key = `${s.name}\t${s.instance}`;
              return (
                <label key={key}>
                  <input type="checkbox" checked={draft.checks.includes(key)} onChange={() => set("checks", toggle(draft.checks, key))} />
                  {checkName(s)}
                </label>
              );
            })}
          </div>
          <span className={admin.fieldNote}>{t("maintenance.form.checksNote")}</span>
        </fieldset>
      )}
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("maintenance.form.repeat")}
          <select className={fieldClass} value={draft.repeat} onChange={(e) => set("repeat", e.target.value as MaintenanceRepeat)}>
            {(["weekly", "monthly", "daily"] as const).map((r) => (
              <option key={r} value={r}>
                {t(`maintenance.form.repeat.${r}`)}
              </option>
            ))}
          </select>
        </label>
        {draft.repeat !== "monthly" && (
          <label className={admin.field}>
            {t("maintenance.form.every")} ({draft.repeat === "daily" ? t("maintenance.form.everyDays") : t("maintenance.form.everyWeeks")})
            <input className={fieldClass} type="number" min={1} max={draft.repeat === "daily" ? 365 : 52} value={draft.every} onChange={(e) => set("every", Number(e.target.value) || 1)} />
          </label>
        )}
      </div>
      {draft.repeat === "weekly" && (
        <fieldset className={admin.fieldset}>
          <legend className={admin.legend}>{t("maintenance.form.days")}</legend>
          <div className={styles.choices}>
            {WEEKDAY_NAMES.map((d) => (
              <label key={d}>
                <input type="checkbox" checked={draft.weekdays.includes(d)} onChange={() => set("weekdays", toggle(draft.weekdays, d))} />
                {dayName(d)}
              </label>
            ))}
          </div>
        </fieldset>
      )}
      {draft.repeat === "monthly" && (
        <>
          <fieldset className={admin.fieldset}>
            <legend className={admin.legend}>{t("maintenance.form.monthBy")}</legend>
            <SegmentedControl
              label={t("maintenance.form.monthBy")}
              options={[
                { value: "day", label: t("maintenance.form.monthByDay") },
                { value: "week", label: t("maintenance.form.monthByWeek") },
              ]}
              value={draft.byWeek ? "week" : "day"}
              onChange={(v) =>
                v === "day"
                  ? set("byWeek", false)
                  : setDraft((d) => ({ ...d, byWeek: true, weekdays: d.weekdays.slice(0, 1).length ? d.weekdays.slice(0, 1) : ["tuesday"] }))
              }
            />
          </fieldset>
          <div className={admin.formGrid}>
            {draft.byWeek ? (
              <>
                <label className={admin.field}>
                  {t("maintenance.form.monthWeek")}
                  <select className={fieldClass} value={draft.monthWeek} onChange={(e) => set("monthWeek", Number(e.target.value))}>
                    {([1, 2, 3, 4, 5] as const).map((w) => (
                      <option key={w} value={w}>
                        {t(`maintenance.week.${w}`)}
                      </option>
                    ))}
                  </select>
                </label>
                <label className={admin.field}>
                  {t("maintenance.form.weekday")}
                  <select className={fieldClass} value={draft.weekdays[0] ?? "tuesday"} onChange={(e) => set("weekdays", [e.target.value as WeekdayName])}>
                    {WEEKDAY_NAMES.map((d) => (
                      <option key={d} value={d}>
                        {dayName(d)}
                      </option>
                    ))}
                  </select>
                </label>
              </>
            ) : (
              <label className={admin.field}>
                {t("maintenance.form.monthDay")}
                <input className={fieldClass} type="number" min={1} max={31} value={draft.monthDay} onChange={(e) => set("monthDay", Number(e.target.value) || 1)} />
              </label>
            )}
          </div>
          <fieldset className={admin.fieldset}>
            <legend className={admin.legend}>{t("maintenance.form.months")}</legend>
            <div className={styles.choices}>
              <Switch
                checked={draft.months.length === 0}
                onChange={(every) => set("months", every ? [] : [new Date().getMonth() + 1])}
                label={t("maintenance.form.everyMonth")}
              />
              {draft.months.length > 0 &&
                MONTHS.map((m) => (
                  <label key={m}>
                    <input type="checkbox" checked={draft.months.includes(m)} onChange={() => set("months", toggle(draft.months, m))} />
                    {monthName(m)}
                  </label>
                ))}
            </div>
          </fieldset>
        </>
      )}
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("maintenance.form.from")}
          <input className={fieldClass} type="time" required value={draft.from} onChange={(e) => set("from", e.target.value)} />
        </label>
        <label className={admin.field}>
          {t("maintenance.form.to")}
          <input
            className={fieldClass}
            type="time"
            required
            value={draft.to}
            onChange={(e) => {
              const to = e.target.value;
              // An end before the start is the next day, as people mean "22:00 to 02:00".
              setDraft((d) => ({ ...d, to, endsAfter: d.endsAfter === 0 && minutesOf(to) <= minutesOf(d.from) ? 1 : d.endsAfter }));
            }}
          />
        </label>
        <label className={admin.field}>
          {t("maintenance.form.ends")}
          <select className={fieldClass} value={draft.endsAfter} onChange={(e) => set("endsAfter", Number(e.target.value))}>
            {[0, 1, 2, 3, 4, 5, 6, 7].map((n) => (
              <option key={n} value={n}>
                {n === 0 ? t("maintenance.form.endsSameDay") : n === 1 ? t("maintenance.form.endsNextDay") : t("maintenance.form.endsLater", { count: n })}
              </option>
            ))}
          </select>
        </label>
        <label className={admin.field}>
          {t("maintenance.form.zone")}
          <select className={fieldClass} value={draft.zone} onChange={(e) => set("zone", e.target.value)}>
            {zones.map((z) => (
              <option key={z} value={z}>
                {z.replaceAll("_", " ")}
              </option>
            ))}
          </select>
        </label>
      </div>
      <span className={admin.fieldNote}>{t("maintenance.form.zoneNote")}</span>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("maintenance.form.startsOn")}
          <input className={fieldClass} type="date" value={draft.startsOn} onChange={(e) => set("startsOn", e.target.value)} />
        </label>
        <label className={admin.field}>
          {t("maintenance.form.endsOn")}
          <input className={fieldClass} type="date" value={draft.endsOn} onChange={(e) => set("endsOn", e.target.value)} />
          <span className={admin.fieldNote}>{t("maintenance.form.endsOnNote")}</span>
        </label>
      </div>
      <label className={admin.field}>
        {t("maintenance.form.why")}
        <input className={fieldClass} required maxLength={200} value={draft.reason} onChange={(e) => set("reason", e.target.value)} />
      </label>
      {typeof request !== "string" && <p className={admin.fieldNote}>{t("maintenance.form.preview", { rule: describeRule(request) })}</p>}
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy || !draft.siteId}>
          {busy ? t("maintenance.form.saving") : window ? t("maintenance.form.save") : t("maintenance.form.create")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("maintenance.form.cancel")}
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

/** "Remove the maintenance window …?", confirmed in place under its row. */
function RemoveWindow({ window, onRemoved, onClose }: { window: MaintenanceWindow; onRemoved: () => void; onClose: () => void }) {
  return (
    <InlineConfirm
      layout="row"
      trigger={t("maintenance.remove")}
      title={t("maintenance.remove.title", { reason: window.reason })}
      consequence={t("maintenance.remove.confirm")}
      confirmLabel={t("maintenance.remove.yes")}
      describeError={describeError}
      onClose={onClose}
      onConfirm={async () => {
        await api.removeMaintenanceWindow(window.id);
        onRemoved();
      }}
    />
  );
}

/** "In maintenance until 04:00 today" while one runs, else the next window. */
function When({ window, now }: { window: MaintenanceWindow; now: Date }) {
  return window.active_until ? (
    <Label tone="warn">{t("maintenance.runningUntil", { when: formatDay(new Date(window.active_until), now) })}</Label>
  ) : (
    <>{describeNext(window, now)}</>
  );
}

const loadAll = (signal: AbortSignal) => api.maintenanceWindows({}, signal);

/** The Maintenance windows part of the Silences page: every window the user reads, and a way to add one. */
export function MaintenanceWindowsSection() {
  const me = useMe();
  const scope = useScope();
  const { data, error, refresh } = usePolledData(loadAll);
  const [editing, setEditing] = useState<MaintenanceWindow | "new" | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const now = new Date();
  const windows = (data?.windows ?? []).filter(
    (w) => (!scope.customerId || w.customer_id === scope.customerId) && (!scope.siteId || w.site_id === scope.siteId),
  );
  const canAdd = me?.roles.some((grant) => grant.role !== "viewer") ?? false;
  const saved = () => {
    setEditing(null);
    refresh();
  };
  const columns: Column<MaintenanceWindow>[] = [
    { key: "what", header: t("maintenance.column.what"), cell: (w) => describeWhat(w) },
    { key: "when", header: t("maintenance.column.when"), cell: (w) => describeRule(w) },
    { key: "next", header: t("maintenance.column.next"), cell: (w) => <When window={w} now={now} /> },
    { key: "why", header: t("maintenance.column.why"), cell: (w) => w.reason },
  ];
  return (
    <section aria-label={t("maintenance.title")} className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("maintenance.title")}</h2>
        {canAdd && editing === null && (
          <Button variant="k" onClick={() => setEditing("new")}>
            {t("maintenance.add")}
          </Button>
        )}
      </div>
      <p className={admin.fieldNote}>{t("maintenance.intro")}</p>
      {editing !== null && (
        <WindowForm
          window={editing === "new" ? null : editing}
          place={scope.siteId ? { siteId: scope.siteId, serverIds: [] } : null}
          onSaved={saved}
          onCancel={() => setEditing(null)}
        />
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("maintenance.load")} error={describeError(error)} onRetry={refresh} />
      ) : data && windows.length === 0 ? (
        <EmptyState>{t("maintenance.empty")}</EmptyState>
      ) : (
        <DataTable
          label={t("maintenance.title")}
          columns={columns}
          rows={windows}
          rowKey={(w) => w.id}
          loading={!data}
          rowActions={(w) =>
            w.can_change && (
              <RowMenu
                label={t("maintenance.actions", { reason: w.reason })}
                items={[
                  { label: t("maintenance.change"), onSelect: () => setEditing(w) },
                  { label: t("maintenance.remove"), onSelect: () => setRemoving(w.id), danger: true },
                ]}
              />
            )
          }
          rowDetail={(w) =>
            removing === w.id && <RemoveWindow window={w} onRemoved={refresh} onClose={() => setRemoving(null)} />
          }
        />
      )}
    </section>
  );
}

/**
 * The windows covering one server or site, as notices for its page, and the
 * form a "Maintenance window…" menu item opens there.
 */
export function useWindowNotices(target: { scope: "server" | "site"; id: string; siteId: string } | null): {
  notices: ReactNode;
  form: ReactNode;
  openForm: () => void;
} {
  const scope = target?.scope;
  const id = target?.id ?? "";
  const fetcher = useCallback(
    (signal: AbortSignal) => api.maintenanceWindows(scope === "server" ? { server_id: id } : { site_id: id }, signal),
    [scope, id],
  );
  const { data, refresh } = usePolledData(fetcher, { enabled: target !== null });
  const [open, setOpen] = useState(false);
  const now = new Date();
  // A running window already shows as "In maintenance until": only the rule is said here.
  const notices = (data?.windows ?? []).map((w) => (
    <Banner key={w.id} tone="plain">
      {w.checks?.length
        ? t("maintenance.bar.windowChecks", { checks: w.checks.map(checkName).join(", "), rule: describeRule(w), next: describeNext(w, now) })
        : t("maintenance.bar.window", { rule: describeRule(w), next: describeNext(w, now) })}
    </Banner>
  ));
  const form = open && target && (
    <WindowForm
      window={null}
      place={{ siteId: target.siteId, serverIds: target.scope === "server" ? [target.id] : [] }}
      onSaved={() => {
        setOpen(false);
        refresh();
      }}
      onCancel={() => setOpen(false)}
    />
  );
  return { notices: <>{notices}</>, form, openForm: () => setOpen(true) };
}
