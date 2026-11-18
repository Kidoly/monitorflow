"use client";

import { type FormEvent, type ReactNode, useCallback, useState } from "react";

import { t } from "@/i18n";
import { type Silence, type SilenceRequest, type SilenceScope, type SilencedCheck, api, describeError, isUnauthorized } from "@/lib/api";
import { customerWide } from "@/lib/customers";
import { administersEverything, modifiesCustomer, modifiesSite, useMe, useMultiCustomer } from "@/lib/me";
import { useScope } from "@/lib/scope";
import { checkName } from "@/lib/maintenance";
import { MAX_DAYS, defaultReason, fromLocalInput, inOneHour, silenceTitle, tomorrowMorning } from "@/lib/silences";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { InfoTip } from "@/components/ui/Hint";
import { RowMenu } from "@/components/ui/RowMenu";
import { formatDay } from "@/components/ui/time";

import admin from "./AdminPages.module.css";
import styles from "./Silences.module.css";

/** What a server, relay or site page can silence. */
export interface SilenceTarget {
  scope: Extract<SilenceScope, "server" | "relay" | "site">;
  id: string;
  name: string;
  customerId: string;
  siteId: string;
}

/**
 * The way to silence a form's values: reason, an optional start, an end.
 * `onSubmit` rejecting keeps the form open with the error.
 */
function SilenceForm({
  submitLabel,
  onSubmit,
  onCancel,
  children,
}: {
  submitLabel: string;
  onSubmit: (period: Pick<SilenceRequest, "starts_at" | "ends_at" | "reason">) => Promise<void>;
  onCancel?: () => void;
  children?: React.ReactNode;
}) {
  const [reason, setReason] = useState(defaultReason);
  const [from, setFrom] = useState("");
  const [until, setUntil] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const end = fromLocalInput(until);
    if (!end) return setError(t("silences.form.noEnd"));
    setBusy(true);
    setError(null);
    try {
      await onSubmit({ starts_at: fromLocalInput(from)?.toISOString(), ends_at: end.toISOString(), reason });
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} onSubmit={(e) => void submit(e)}>
      {children}
      <label className={admin.field}>
        {t("silences.form.why")}
        <input className={fieldClass} required maxLength={200} value={reason} onChange={(e) => setReason(e.target.value)} />
      </label>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("silences.form.from")}
          <input className={fieldClass} type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
          <span className={admin.fieldNote}>{t("silences.form.fromNote")}</span>
        </label>
        <label className={admin.field}>
          {t("silences.form.until")}
          <input className={fieldClass} type="datetime-local" required value={until} onChange={(e) => setUntil(e.target.value)} />
          <span className={admin.fieldNote}>{t("silences.form.untilNote", { count: MAX_DAYS })}</span>
        </label>
      </div>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("silences.form.saving") : submitLabel}
        </Button>
        {onCancel && (
          <Button onClick={onCancel} disabled={busy}>
            {t("silences.form.cancel")}
          </Button>
        )}
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

/** "End now" or, before it starts, "Call off"; the list refreshes after. */
function EndSilence({ silence, onEnded }: { silence: Silence; onEnded: () => void }) {
  const multiCustomer = useMultiCustomer();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function end() {
    setBusy(true);
    setError(null);
    try {
      await api.endSilence(silence.id);
      onEnded();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }
  return (
    <>
      <Button size="small" disabled={busy} onClick={() => void end()} aria-label={t("silences.end.label", { name: silenceTitle(silence, multiCustomer) })}>
        {silence.active ? t("silences.end.now") : t("silences.end.callOff")}
      </Button>
      {error && <span role="alert"> {error}</span>}
    </>
  );
}

/** "Planned maintenance, by alice": who answers for it. */
function because(silence: Silence): string {
  return t("silences.because", { reason: silence.reason, name: silence.created_by_username ?? t("silences.removedUser") });
}

/**
 * A silenced server, relay or site says so at the top of its page, and
 * operators of its site silence it from here in one click.
 */
/**
 * The silences of one server, relay or site: notices for active and planned
 * ones, a "Silence ▾" menu (1 hour, until tomorrow 8:00, custom), and the
 * custom form. A page that puts the menu in its header uses this hook;
 * `SilenceBar` stacks the three.
 */
export type SilenceBarParts = ReturnType<typeof useSilenceBar>;

/** As [`useSilenceBar`], before the page has loaded what it silences: nothing until then. */
export function useSilenceBar(target: SilenceTarget | null): {
  notices: ReactNode;
  menu: ReactNode;
  form: ReactNode;
  /** Active silences of chosen checks (ADR-0151), for the check rows. */
  checkSilences: Silence[];
  /** Silences `check` until `end` in one click; rejects with the API's error. */
  silenceCheck: (check: SilencedCheck, end: Date) => Promise<void>;
} {
  const multiCustomer = useMultiCustomer();
  const me = useMe();
  const { scope, id, name } = target ?? { scope: "server" as const, id: "", name: "" };
  const key = scope === "server" ? "server_id" : scope === "relay" ? "relay_id" : "site_id";
  const fetcher = useCallback((signal: AbortSignal) => api.silences({ [key]: id }, signal), [key, id]);
  const { data, refresh } = usePolledData(fetcher, { enabled: target !== null });
  const [custom, setCustom] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const silences = data?.silences ?? [];
  // A silence of chosen checks leaves the rest of the page's target watched.
  const active = silences.filter((s) => s.active && !s.checks);
  const checkSilences = silences.filter((s) => s.active && s.checks);
  const upcoming = silences.filter((s) => !s.active);
  const now = new Date();
  const last = active.reduce<Silence | null>((a, s) => (a && a.ends_at > s.ends_at ? a : s), null);

  async function silence(period: Pick<SilenceRequest, "starts_at" | "ends_at" | "reason">) {
    await api.createSilence({ scope, target_id: id, ...period });
    setCustom(false);
    refresh();
  }
  async function silenceCheck(check: SilencedCheck, end: Date) {
    await api.createSilence({ scope, target_id: id, ends_at: end.toISOString(), reason: defaultReason(), checks: [check] });
    refresh();
  }
  async function until(end: Date) {
    setBusy(true);
    setError(null);
    try {
      await silence({ ends_at: end.toISOString(), reason: defaultReason() });
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  const notices = (
    <>
      {last && (
        <Banner tone="warn">
          <strong>
            {last.window_id
              ? t("maintenance.runningUntil", { when: formatDay(new Date(last.ends_at), now) }) + "."
              : t("silences.bar.until", { when: formatDay(new Date(last.ends_at), now) })}
          </strong>{" "}
          {t("silences.bar.nobody", { name })}
          <ul className={styles.lines}>
            {active.map((s) => (
              <li key={s.id}>
                {s.scope === scope ? "" : t("silences.bar.with", { name: silenceTitle(s, multiCustomer) })}
                {t("silences.bar.line", { because: because(s), when: formatDay(new Date(s.ends_at), now) })}{" "}
                {s.can_end && <EndSilence silence={s} onEnded={refresh} />}
              </li>
            ))}
          </ul>
        </Banner>
      )}
      {checkSilences.length > 0 && (
        <Banner tone="plain">
          {t("maintenance.check.bar", { checks: [...new Set(checkSilences.flatMap((s) => s.checks ?? []).map(checkName))].join(", ") })}
          <ul className={styles.lines}>
            {checkSilences.map((s) => (
              <li key={s.id}>
                {t("maintenance.check.line", {
                  checks: (s.checks ?? []).map(checkName).join(", "),
                  when: formatDay(new Date(s.ends_at), now),
                  because: because(s),
                })}{" "}
                {s.can_end && <EndSilence silence={s} onEnded={refresh} />}
              </li>
            ))}
          </ul>
        </Banner>
      )}
      {upcoming.map((s) => (
        <Banner key={s.id} tone="plain">
          {s.scope === scope
            ? t("silences.bar.planned", {
                from: formatDay(new Date(s.starts_at), now),
                to: formatDay(new Date(s.ends_at), now),
                because: because(s),
              })
            : t("silences.bar.plannedWith", {
                name: silenceTitle(s, multiCustomer),
                from: formatDay(new Date(s.starts_at), now),
                to: formatDay(new Date(s.ends_at), now),
                because: because(s),
              })}{" "}
          {s.can_end && <EndSilence silence={s} onEnded={refresh} />}
        </Banner>
      ))}
    </>
  );
  const menu = target !== null && modifiesSite(me, target.customerId, target.siteId) && (
    <span className={styles.bar}>
      <RowMenu
        label={t("silences.bar.label", { name })}
        trigger={last ? t("silences.bar.longer") : t("silences.bar.trigger")}
        disabled={busy}
        items={[
          { label: t("silences.bar.hour"), onSelect: () => void until(inOneHour(new Date())) },
          { label: t("silences.bar.tomorrow"), onSelect: () => void until(tomorrowMorning(new Date())) },
          { label: t("silences.bar.custom"), onSelect: () => setCustom(true) },
        ]}
      />
      {error && <span role="alert">{error}</span>}
    </span>
  );
  const form = custom && (
    <SilenceForm submitLabel={t("silences.bar.submit", { name })} onSubmit={silence} onCancel={() => setCustom(false)} />
  );
  return { notices, menu, form, checkSilences, silenceCheck };
}

/** Notices, the "Silence ▾" menu and the custom form, one under the other. */
export function SilenceBar({ target }: { target: SilenceTarget }) {
  const { notices, menu, form } = useSilenceBar(target);
  return (
    <>
      {notices}
      {menu && <div>{menu}</div>}
      {form}
    </>
  );
}

/** What the user may silence, as options of a select. */
interface Choice {
  value: string;
  label: string;
  scope: SilenceScope;
  id?: string;
}

async function loadChoices(signal: AbortSignal) {
  const [{ customers }, { sites }, { servers }] = await Promise.all([api.customers(signal), api.sites(signal), api.servers(signal)]);
  return { customers, sites, servers };
}

/** Choosing anything that can be silenced, for what has no page of its own. */
function NewSilence({ onCreated, onCancel }: { onCreated: () => void; onCancel: () => void }) {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const everything = administersEverything(me);
  const { data } = usePolledData(loadChoices);
  const [value, setValue] = useState("");
  const choices: Choice[] = [
    ...(everything ? [{ value: "all", label: t("silences.choice.everything"), scope: "all" as const }] : []),
    // For a single company, Everything says it all; a customer's user gets "All sites" (ADR-0104).
    ...(data?.customers ?? [])
      .filter((c) => modifiesCustomer(me, c.id) && (multiCustomer || !everything))
      .map((c) => ({
        value: `customer/${c.id}`,
        label: customerWide(t("silences.choice.customer", { name: c.name }), multiCustomer),
        scope: "customer" as const,
        id: c.id,
      })),
    ...(data?.sites ?? [])
      .filter((s) => modifiesSite(me, s.customer_id, s.id))
      .map((s) => ({ value: `site/${s.id}`, label: t("silences.choice.site", { name: s.name }), scope: "site" as const, id: s.id })),
    ...(data?.servers ?? [])
      .filter((s) => modifiesSite(me, s.customer_id, s.site_id))
      .map((s) => ({ value: `server/${s.id}`, label: t("silences.choice.server", { name: s.hostname }), scope: "server" as const, id: s.id })),
  ];
  // Nothing chosen at first: a wide silence is never a default.
  const chosen = choices.find((c) => c.value === value);
  return (
    <SilenceForm
      submitLabel={t("silences.submit")}
      onCancel={onCancel}
      onSubmit={async (period) => {
        if (!chosen) return;
        await api.createSilence({ scope: chosen.scope, target_id: chosen.id, ...period });
        onCreated();
      }}
    >
      <label className={admin.field}>
        {t("silences.form.what")}
        <select className={fieldClass} required value={value} onChange={(e) => setValue(e.target.value)}>
          <option value="" disabled>
            {t("silences.form.choose")}
          </option>
          {choices.map((c) => (
            <option key={c.value} value={c.value}>
              {c.label}
            </option>
          ))}
        </select>
      </label>
    </SilenceForm>
  );
}

const load = (signal: AbortSignal) => api.silences({}, signal);

/** `/silences/`: what is silenced now and what is planned, for the sites the user sees. */
export function SilencesPage() {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const scope = useScope();
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);
  const [adding, setAdding] = useState(false);
  const now = updatedAt ?? new Date();
  // "Everything" belongs to every customer and site of the switcher.
  // A maintenance window's running occurrence shows with its window.
  const silences = (data?.silences ?? []).filter(
    (s) =>
      !s.window_id &&
      (!scope.customerId || !s.customer_id || s.customer_id === scope.customerId) &&
      (!scope.siteId || !s.site_id || s.site_id === scope.siteId),
  );
  const canSilence = me?.roles.some((grant) => grant.role !== "viewer") ?? false;
  const columns: Column<Silence>[] = [
    {
      key: "what",
      header: t("silences.column.what"),
      cell: (s) =>
        s.checks?.length
          ? t("maintenance.what.checks", { what: silenceTitle(s, multiCustomer), checks: s.checks.map(checkName).join(", ") })
          : silenceTitle(s, multiCustomer),
    },
    {
      key: "state",
      header: t("silences.column.state"),
      cell: (s) => <Label tone={s.active ? "warn" : "neutral"}>{s.active ? t("silences.active") : t("silences.upcoming")}</Label>,
    },
    { key: "from", header: t("silences.column.from"), cell: (s) => formatDay(new Date(s.starts_at), now) },
    { key: "until", header: t("silences.column.until"), cell: (s) => formatDay(new Date(s.ends_at), now) },
    { key: "why", header: t("silences.column.why"), cell: (s) => s.reason },
    { key: "by", header: t("silences.column.by"), cell: (s) => s.created_by_username ?? t("silences.removedUserCell") },
    { key: "end", header: "", cell: (s) => s.can_end && <EndSilence silence={s} onEnded={refresh} /> },
  ];
  return (
    <Page>
      <PageHeader
        title={t("silences.title")}
        status={<InfoTip label={t("silences.about")}>{t("silences.intro")}</InfoTip>}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={
          canSilence && (
            <Button variant="k" aria-expanded={adding} onClick={() => setAdding(!adding)}>
              {t("silences.add")}
            </Button>
          )
        }
      />
      {adding && (
        <NewSilence
          onCancel={() => setAdding(false)}
          onCreated={() => {
            setAdding(false);
            refresh();
          }}
        />
      )}
      {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("silences.load")} error={describeError(error)} onRetry={refresh} />
      ) : data && silences.length === 0 ? (
        <EmptyState>{t("silences.empty")}</EmptyState>
      ) : (
        <DataTable label={t("silences.title")} columns={columns} rows={silences} rowKey={(s) => s.id} loading={!data} />
      )}
    </Page>
  );
}
