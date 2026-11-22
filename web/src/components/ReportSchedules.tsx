"use client";

import { type FormEvent, useCallback, useState } from "react";

import { t } from "@/i18n";
import { activeLocale } from "@/i18n/locale";
import { type Customer, type NotificationChannel, type ReportSchedule, type ReportScheduleRequest, api, describeError, isUnauthorized } from "@/lib/api";
import { administersCustomer, administersEverything, isAdministrator, useMe, useMultiCustomer } from "@/lib/me";
import { browserTimeZone, timeZones } from "@/lib/onCall";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { RowMenu } from "@/components/ui/RowMenu";
import { Banner, EmptyState, ErrorState } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { formatExact } from "@/components/ui/time";
import { Switch } from "@/components/ui/Switch";

import admin from "./AdminPages.module.css";
import styles from "./Reports.module.css";

/** "Everything" in the customer choice. */
const EVERYTHING = "";

interface Draft {
  customerId: string;
  frequency: "monthly" | "weekly";
  zone: string;
  language: "en" | "fr";
  pdf: boolean;
  csv: boolean;
  channelId: string;
  recipients: string;
  enabled: boolean;
}

function draftOf(schedule: ReportSchedule | null, customerId: string): Draft {
  if (!schedule) {
    return {
      customerId,
      frequency: "monthly",
      zone: browserTimeZone(),
      language: activeLocale() === "fr" ? "fr" : "en",
      pdf: true,
      csv: true,
      channelId: "",
      recipients: "",
      enabled: true,
    };
  }
  return {
    customerId: schedule.customer_id ?? EVERYTHING,
    frequency: schedule.frequency,
    zone: schedule.time_zone,
    language: schedule.language,
    pdf: schedule.pdf,
    csv: schedule.csv,
    channelId: schedule.channel_id,
    recipients: schedule.recipients.join(", "),
    enabled: schedule.enabled,
  };
}

/** The draft as the API takes it, or why not. */
export function scheduleRequestOf(draft: Draft): ReportScheduleRequest | string {
  if (!draft.channelId) return t("reports.schedule.noChannel");
  if (!draft.pdf && !draft.csv) return t("reports.schedule.noFile");
  return {
    customer_id: draft.customerId || null,
    frequency: draft.frequency,
    time_zone: draft.zone,
    language: draft.language,
    pdf: draft.pdf,
    csv: draft.csv,
    channel_id: draft.channelId,
    recipients: draft.recipients
      .split(/[\s,;]+/)
      .map((r) => r.trim())
      .filter(Boolean),
    enabled: draft.enabled,
  };
}

/** Email channels that can send a report of `customerId`: of that customer as a whole, or of everything. */
export function channelsFor(channels: NotificationChannel[], customerId: string): NotificationChannel[] {
  return channels.filter(
    (c) => c.kind === "email" && c.site_id === null && (c.customer_id === null || (customerId !== EVERYTHING && c.customer_id === customerId)),
  );
}

const channelName = (c: NotificationChannel) => (c.kind === "email" ? `${c.settings.host} (${c.settings.to.join(", ")})` : c.id);

function ScheduleForm({
  schedule,
  customers,
  channels,
  onSaved,
  onCancel,
}: {
  schedule: ReportSchedule | null;
  customers: Customer[];
  channels: NotificationChannel[];
  onSaved: () => void;
  onCancel: () => void;
}) {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const everything = administersEverything(me);
  const [draft, setDraft] = useState(() => draftOf(schedule, everything ? EVERYTHING : (customers[0]?.id ?? EVERYTHING)));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }));
  const usable = channelsFor(channels, draft.customerId);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const request = scheduleRequestOf(draft);
    if (typeof request === "string") {
      setError(request);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (schedule) await api.updateReportSchedule(schedule.id, request);
      else await api.createReportSchedule(request);
      onSaved();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className={styles.form} onSubmit={submit} aria-label={schedule ? t("reports.schedule.changeLabel") : t("reports.schedule.add")}>
      {multiCustomer && (
        <label className={admin.field}>
          {t("reports.schedule.for")}
          <select className={fieldClass} value={draft.customerId} onChange={(e) => setDraft({ ...draft, customerId: e.target.value, channelId: "" })}>
            {everything && <option value={EVERYTHING}>{t("reports.schedule.everything")}</option>}
            {customers.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("reports.schedule.frequency")}
          <select className={fieldClass} value={draft.frequency} onChange={(e) => set("frequency", e.target.value as Draft["frequency"])}>
            <option value="monthly">{t("reports.schedule.monthly")}</option>
            <option value="weekly">{t("reports.schedule.weekly")}</option>
          </select>
        </label>
        <label className={admin.field}>
          {t("reports.schedule.language")}
          <select className={fieldClass} value={draft.language} onChange={(e) => set("language", e.target.value as Draft["language"])}>
            <option value="en">English</option>
            <option value="fr">Français</option>
          </select>
        </label>
        <label className={admin.field}>
          {t("reports.schedule.zone")}
          <select className={fieldClass} value={draft.zone} onChange={(e) => set("zone", e.target.value)}>
            {timeZones(draft.zone).map((z) => (
              <option key={z} value={z}>
                {z.replaceAll("_", " ")}
              </option>
            ))}
          </select>
        </label>
      </div>
      <span className={admin.fieldNote}>{t("reports.schedule.whenNote")}</span>
      <label className={admin.field}>
        {t("reports.schedule.channel")}
        <select className={fieldClass} value={draft.channelId} onChange={(e) => set("channelId", e.target.value)}>
          <option value="">{t("reports.service.choose")}</option>
          {usable.map((c) => (
            <option key={c.id} value={c.id}>
              {channelName(c)}
            </option>
          ))}
        </select>
        {usable.length === 0 && <span className={admin.fieldNote}>{t("reports.schedule.noChannelYet")}</span>}
      </label>
      <label className={admin.field}>
        {t("reports.schedule.recipients")}
        <input className={fieldClass} value={draft.recipients} placeholder="it@example.com" onChange={(e) => set("recipients", e.target.value)} />
        <span className={admin.fieldNote}>{t("reports.schedule.recipientsNote")}</span>
      </label>
      <div className={styles.choices}>
        <Switch checked={draft.pdf} onChange={(on) => set("pdf", on)} label="PDF" />
        <Switch checked={draft.csv} onChange={(on) => set("csv", on)} label="CSV" />
        <Switch checked={draft.enabled} onChange={(on) => set("enabled", on)} label={t("reports.schedule.enabled")} />
      </div>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("reports.saving") : schedule ? t("reports.save") : t("reports.schedule.create")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("reports.cancel")}
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

const loadSchedules = (signal: AbortSignal) => api.reportSchedules(signal);
const when = (iso: string | null) => (iso ? formatExact(new Date(iso)).slice(0, 16) : t("reports.never"));

/** Reports sent by email on a schedule, for administrators: monthly by default (ADR-0155). */
export function ReportSchedulesSection() {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const enabled = isAdministrator(me);
  const { data, error, refresh } = usePolledData(loadSchedules, { enabled, intervalMs: 60_000 });
  const [editing, setEditing] = useState<ReportSchedule | "new" | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [sent, setSent] = useState<{ id: string; text: string; ok: boolean } | null>(null);
  const [places, setPlaces] = useState<{ customers: Customer[]; channels: NotificationChannel[] } | null>(null);
  const loadPlaces = useCallback(async () => {
    const [customers, channels] = await Promise.all([api.customers(), api.notificationChannels()]);
    setPlaces({ customers: customers.customers.filter((c) => administersCustomer(me, c.id)), channels: channels.channels });
  }, [me]);
  if (!enabled) return null;
  const open = (target: ReportSchedule | "new") => {
    setEditing(target);
    if (!places) void loadPlaces().catch(() => setPlaces({ customers: [], channels: [] }));
  };
  const saved = () => {
    setEditing(null);
    refresh();
  };
  const sendNow = async (schedule: ReportSchedule) => {
    setSent({ id: schedule.id, text: t("reports.schedule.sending"), ok: true });
    try {
      const answer = await api.sendReportNow(schedule.id);
      setSent({
        id: schedule.id,
        ok: answer.ok,
        text: answer.ok ? t("reports.schedule.sent", { to: answer.recipients.join(", ") }) : t("reports.schedule.failed", { error: answer.error ?? "" }),
      });
    } catch (e) {
      setSent({ id: schedule.id, ok: false, text: describeError(e) });
    }
    refresh();
  };
  const columns: Column<ReportSchedule>[] = [
    ...(multiCustomer
      ? [{ key: "for", header: t("reports.schedule.for"), cell: (s: ReportSchedule) => s.customer_name ?? t("reports.schedule.everything") }]
      : []),
    {
      key: "when",
      header: t("reports.schedule.frequency"),
      cell: (s) => (
        <>
          {s.frequency === "monthly" ? t("reports.schedule.monthly") : t("reports.schedule.weekly")}
          {!s.enabled && (
            <>
              {" "}
              <Label tone="neutral">{t("reports.schedule.off")}</Label>
            </>
          )}
        </>
      ),
    },
    { key: "next", header: t("reports.schedule.next"), cell: (s) => when(s.next_send_at) },
    { key: "to", header: t("reports.schedule.to"), cell: (s) => (s.recipients.length ? s.recipients : s.channel_recipients).join(", ") },
    {
      key: "last",
      header: t("reports.schedule.last"),
      cell: (s) => (s.last_error ? <Label tone="bad">{t("reports.schedule.failed", { error: s.last_error })}</Label> : when(s.last_sent_at)),
    },
  ];
  return (
    <section aria-label={t("reports.schedule.title")} className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("reports.schedule.title")}</h2>
        {editing === null && (
          <Button variant="k" onClick={() => open("new")}>
            {t("reports.schedule.add")}
          </Button>
        )}
      </div>
      <p className={admin.fieldNote}>{t("reports.schedule.intro")}</p>
      {editing !== null &&
        (places ? (
          <ScheduleForm schedule={editing === "new" ? null : editing} customers={places.customers} channels={places.channels} onSaved={saved} onCancel={() => setEditing(null)} />
        ) : (
          <p className={admin.fieldNote}>{t("reports.loading")}</p>
        ))}
      {sent && <Banner tone={sent.ok ? "plain" : "bad"}>{sent.text}</Banner>}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("reports.schedule.load")} error={describeError(error)} onRetry={refresh} />
      ) : data && data.schedules.length === 0 ? (
        <EmptyState>{t("reports.schedule.empty")}</EmptyState>
      ) : (
        <DataTable
          label={t("reports.schedule.title")}
          columns={columns}
          rows={data?.schedules ?? []}
          rowKey={(s) => s.id}
          loading={!data}
          rowActions={(s) => (
            <RowMenu
              label={t("reports.schedule.actions")}
              items={[
                { label: t("reports.schedule.sendNow"), onSelect: () => void sendNow(s) },
                { label: t("reports.change"), onSelect: () => open(s) },
                { label: t("reports.remove"), onSelect: () => setRemoving(s.id), danger: true },
              ]}
            />
          )}
          rowDetail={(s) =>
            removing === s.id && (
              <InlineConfirm
                layout="row"
                trigger={t("reports.remove")}
                title={t("reports.schedule.removeTitle")}
                consequence={t("reports.schedule.removeConsequence")}
                confirmLabel={t("reports.remove")}
                describeError={describeError}
                onClose={() => setRemoving(null)}
                onConfirm={async () => {
                  await api.removeReportSchedule(s.id);
                  saved();
                }}
              />
            )
          }
        />
      )}
    </section>
  );
}
