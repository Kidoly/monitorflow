"use client";

import { type FormEvent, useCallback, useState } from "react";

import { type Silence, type SilenceRequest, type SilenceScope, api, describeError, isUnauthorized } from "@/lib/api";
import { administersEverything, modifiesCustomer, modifiesSite, useMe } from "@/lib/me";
import { useScope } from "@/lib/scope";
import { DEFAULT_REASON, MAX_DAYS, fromLocalInput, inOneHour, silenceTitle, tomorrowMorning } from "@/lib/silences";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
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
  const [reason, setReason] = useState(DEFAULT_REASON);
  const [from, setFrom] = useState("");
  const [until, setUntil] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const end = fromLocalInput(until);
    if (!end) return setError("Say when it ends. Every silence ends on its own.");
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
        Why
        <input className={fieldClass} required maxLength={200} value={reason} onChange={(e) => setReason(e.target.value)} />
      </label>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          From
          <input className={fieldClass} type="datetime-local" value={from} onChange={(e) => setFrom(e.target.value)} />
          <span className={admin.fieldNote}>Empty is now.</span>
        </label>
        <label className={admin.field}>
          Until
          <input className={fieldClass} type="datetime-local" required value={until} onChange={(e) => setUntil(e.target.value)} />
          <span className={admin.fieldNote}>At most {MAX_DAYS} days.</span>
        </label>
      </div>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? "Saving…" : submitLabel}
        </Button>
        {onCancel && (
          <Button onClick={onCancel} disabled={busy}>
            Cancel
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
      <Button size="small" disabled={busy} onClick={() => void end()} aria-label={`End the silence of ${silenceTitle(silence)}`}>
        {silence.active ? "End now" : "Call off"}
      </Button>
      {error && <span role="alert"> {error}</span>}
    </>
  );
}

/** "Planned maintenance, by alice": who answers for it. */
function because(silence: Silence): string {
  return `${silence.reason}, by ${silence.created_by_username ?? "a removed user"}`;
}

/**
 * A silenced server, relay or site says so at the top of its page, and
 * operators of its site silence it from here in one click.
 */
export function SilenceBar({ target }: { target: SilenceTarget }) {
  const me = useMe();
  const { scope, id, name } = target;
  const key = scope === "server" ? "server_id" : scope === "relay" ? "relay_id" : "site_id";
  const fetcher = useCallback((signal: AbortSignal) => api.silences({ [key]: id }, signal), [key, id]);
  const { data, refresh } = usePolledData(fetcher);
  const [custom, setCustom] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const silences = data?.silences ?? [];
  const active = silences.filter((s) => s.active);
  const upcoming = silences.filter((s) => !s.active);
  const now = new Date();
  const last = active.reduce<Silence | null>((a, s) => (a && a.ends_at > s.ends_at ? a : s), null);

  async function silence(period: Pick<SilenceRequest, "starts_at" | "ends_at" | "reason">) {
    await api.createSilence({ scope, target_id: id, ...period });
    setCustom(false);
    refresh();
  }
  async function until(end: Date) {
    setBusy(true);
    setError(null);
    try {
      await silence({ ends_at: end.toISOString(), reason: DEFAULT_REASON });
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      {last && (
        <Banner tone="warn">
          <strong>Silenced until {formatDay(new Date(last.ends_at), now)}.</strong> Nobody is told about {name} until then.
          Incidents still open and show here, and what is still open at the end is sent then.
          <ul className={styles.lines}>
            {active.map((s) => (
              <li key={s.id}>
                {s.scope === scope ? "" : `With ${silenceTitle(s)}: `}
                {because(s)}, until {formatDay(new Date(s.ends_at), now)}. {s.can_end && <EndSilence silence={s} onEnded={refresh} />}
              </li>
            ))}
          </ul>
        </Banner>
      )}
      {upcoming.map((s) => (
        <Banner key={s.id} tone="plain">
          Silence planned{s.scope === scope ? "" : ` with ${silenceTitle(s)}`} from {formatDay(new Date(s.starts_at), now)} to{" "}
          {formatDay(new Date(s.ends_at), now)}: {because(s)}. {s.can_end && <EndSilence silence={s} onEnded={refresh} />}
        </Banner>
      ))}
      {modifiesSite(me, target.customerId, target.siteId) && (
        <div className={styles.bar} role="group" aria-label={`Silence notifications about ${name}`}>
          <span className={styles.label}>{last ? "Silence longer" : "Silence for"}</span>
          <Button size="small" disabled={busy} onClick={() => void until(inOneHour(new Date()))}>
            1 hour
          </Button>
          <Button size="small" disabled={busy} onClick={() => void until(tomorrowMorning(new Date()))}>
            Until tomorrow, 8:00
          </Button>
          <Button size="small" aria-expanded={custom} onClick={() => setCustom(!custom)}>
            Custom…
          </Button>
          {error && <span role="alert">{error}</span>}
        </div>
      )}
      {custom && <SilenceForm submitLabel={`Silence ${name}`} onSubmit={silence} onCancel={() => setCustom(false)} />}
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
  const { data } = usePolledData(loadChoices);
  const [value, setValue] = useState("");
  const choices: Choice[] = [
    ...(administersEverything(me) ? [{ value: "all", label: "Everything", scope: "all" as const }] : []),
    ...(data?.customers ?? [])
      .filter((c) => modifiesCustomer(me, c.id))
      .map((c) => ({ value: `customer/${c.id}`, label: `Customer ${c.name}`, scope: "customer" as const, id: c.id })),
    ...(data?.sites ?? [])
      .filter((s) => modifiesSite(me, s.customer_id, s.id))
      .map((s) => ({ value: `site/${s.id}`, label: `Site ${s.name}`, scope: "site" as const, id: s.id })),
    ...(data?.servers ?? [])
      .filter((s) => modifiesSite(me, s.customer_id, s.site_id))
      .map((s) => ({ value: `server/${s.id}`, label: `Server ${s.hostname}`, scope: "server" as const, id: s.id })),
  ];
  // Nothing chosen at first: a wide silence is never a default.
  const chosen = choices.find((c) => c.value === value);
  return (
    <SilenceForm
      submitLabel="Silence"
      onCancel={onCancel}
      onSubmit={async (period) => {
        if (!chosen) return;
        await api.createSilence({ scope: chosen.scope, target_id: chosen.id, ...period });
        onCreated();
      }}
    >
      <label className={admin.field}>
        What
        <select className={fieldClass} required value={value} onChange={(e) => setValue(e.target.value)}>
          <option value="" disabled>
            Choose…
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
  const scope = useScope();
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);
  const [adding, setAdding] = useState(false);
  const now = updatedAt ?? new Date();
  // "Everything" belongs to every customer and site of the switcher.
  const silences = (data?.silences ?? []).filter(
    (s) =>
      (!scope.customerId || !s.customer_id || s.customer_id === scope.customerId) &&
      (!scope.siteId || !s.site_id || s.site_id === scope.siteId),
  );
  const canSilence = me?.roles.some((grant) => grant.role !== "viewer") ?? false;
  const columns: Column<Silence>[] = [
    { key: "what", header: "What", cell: (s) => silenceTitle(s) },
    {
      key: "state",
      header: "State",
      cell: (s) => <Label tone={s.active ? "warn" : "neutral"}>{s.active ? "Active" : "Upcoming"}</Label>,
    },
    { key: "from", header: "From", cell: (s) => formatDay(new Date(s.starts_at), now) },
    { key: "until", header: "Until", cell: (s) => formatDay(new Date(s.ends_at), now) },
    { key: "why", header: "Why", cell: (s) => s.reason },
    { key: "by", header: "Created by", cell: (s) => s.created_by_username ?? "A removed user" },
    { key: "end", header: "", cell: (s) => s.can_end && <EndSilence silence={s} onEnded={refresh} /> },
  ];
  return (
    <Page>
      <PageHeader
        title="Silences"
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={
          canSilence && (
            <Button variant="k" aria-expanded={adding} onClick={() => setAdding(!adding)}>
              New silence
            </Button>
          )
        }
      />
      <p className={styles.intro}>
        A silence stops notifications for a while, for planned work. Incidents still open and show as usual. When it ends, what is
        still open is sent, once. To silence one server or site, open its page.
      </p>
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
        <ErrorState what="silences" error={describeError(error)} onRetry={refresh} />
      ) : data && silences.length === 0 ? (
        <EmptyState>Nothing is silenced. Notifications go out as usual.</EmptyState>
      ) : (
        <DataTable label="Silences" columns={columns} rows={silences} rowKey={(s) => s.id} loading={!data} />
      )}
    </Page>
  );
}
