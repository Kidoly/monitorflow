"use client";

import Link from "next/link";
import { useSearchParams } from "next/navigation";
import { type FormEvent, useCallback, useMemo, useState } from "react";

import { t } from "@/i18n";
import {
  type Dependency,
  type DependencyEnd,
  type DependencyEndRequest,
  type DependencyPage,
  type IncidentDetail,
  type Relay,
  type Server,
  type Unreachable,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { endHref, endWords } from "@/lib/dependencies";
import { incidentHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, EmptyState, ErrorState } from "@/components/ui/PageState";
import { RowMenu } from "@/components/ui/RowMenu";
import { Label } from "@/components/ui/StatusLabel";
import { formatDay } from "@/components/ui/time";

import admin from "./AdminPages.module.css";
import styles from "./Silences.module.css";

/** A link to an end: "ping on sw-core", "relay gw-1". */
function EndLink({ end }: { end: DependencyEnd }) {
  return <Link href={endHref(end)}>{endWords(end)}</Link>;
}

/**
 * "Unreachable: sw-core is down." on the page of a server or device behind
 * it, with its incident (ADR-0152).
 */
export function UnreachableBanner({ name, note }: { name: string; note: Unreachable }) {
  return (
    <Banner tone="info">
      <strong>{t("dependencies.unreachable.banner", { parent: endWords(note.because) })}</strong>{" "}
      {t("dependencies.unreachable.detail", { name })}{" "}
      <Link href={incidentHref(note.incident_id)}>{t("dependencies.unreachable.incident")}</Link>
    </Banner>
  );
}

/** What depends on the page's place, where it may be set from. */
interface Place {
  kind: "server" | "relay";
  id: string;
  name: string;
  customerId: string;
  /** Its checks' names, for a server or device. */
  checks: string[];
}

/** The tab of a server or device: what it depends on and what is behind it. */
export function DependenciesTab({ server, checks }: { server: Server; checks: string[] }) {
  const place: Place = { kind: "server", id: server.id, name: server.hostname, customerId: server.customer_id, checks };
  return <DependencySections place={place} />;
}

/** The section of a relay's page: what is behind it. */
export function RelayDependencies({ relay }: { relay: Pick<Relay, "id" | "name" | "customer_id"> }) {
  const place: Place = { kind: "relay", id: relay.id, name: relay.name, customerId: relay.customer_id, checks: [] };
  return <DependencySections place={place} />;
}

function DependencySections({ place }: { place: Place }) {
  const fetcher = useCallback(
    (signal: AbortSignal) =>
      place.kind === "server" ? api.serverDependencies(place.id, signal) : api.relayDependencies(place.id, signal),
    [place.kind, place.id],
  );
  const { data, error, refresh } = usePolledData(fetcher);
  if (!data) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what={t("dependencies.load")} error={describeError(error)} onRetry={refresh} />
    ) : null;
  }
  return (
    <>
      {place.kind === "server" && <Parents place={place} page={data} onChanged={refresh} />}
      <Children place={place} page={data} onChanged={refresh} />
    </>
  );
}

/** "All of web-1" or "https on web-1": the part of a child or parent. */
function partWords(end: DependencyEnd): string {
  return end.check || end.kind === "relay" ? endWords(end) : t("dependencies.end.whole", { name: end.name });
}

function Added({ dependency, now }: { dependency: Dependency; now: Date }) {
  if (dependency.automatic) return <Label tone="neutral">{t("dependencies.automatic")}</Label>;
  if (!dependency.created_at) return null;
  const when = formatDay(new Date(dependency.created_at), now);
  return (
    <>{dependency.created_by ? t("dependencies.addedBy", { who: dependency.created_by, when }) : t("dependencies.addedWhen", { when })}</>
  );
}

function Remove({ dependency, onRemoved, onClose }: { dependency: Dependency; onRemoved: () => void; onClose: () => void }) {
  const parent = endWords(dependency.parent);
  return (
    <InlineConfirm
      layout="row"
      trigger={t("dependencies.remove")}
      title={t("dependencies.remove.title", { child: partWords(dependency.child), parent })}
      consequence={t("dependencies.remove.consequence", { parent })}
      confirmLabel={t("dependencies.remove.confirm")}
      pendingLabel={t("dependencies.remove.pending")}
      describeError={(e) => t("dependencies.remove.failed", { error: describeError(e) })}
      onClose={onClose}
      onConfirm={async () => {
        await api.removeDependency(dependency.id!);
        onRemoved();
      }}
    />
  );
}

function DependencyTable({
  label,
  rows,
  side,
  onChanged,
}: {
  label: string;
  rows: Dependency[];
  /** The column showing the other end. */
  side: "parent" | "child";
  onChanged: () => void;
}) {
  const [removing, setRemoving] = useState<string | null>(null);
  const now = new Date();
  const columns: Column<Dependency>[] =
    side === "parent"
      ? [
          { key: "what", header: t("dependencies.column.what"), cell: (d) => partWords(d.child) },
          { key: "parent", header: t("dependencies.column.parent"), cell: (d) => <EndLink end={d.parent} /> },
          { key: "added", header: t("dependencies.column.added"), cell: (d) => <Added dependency={d} now={now} /> },
        ]
      : [
          { key: "child", header: t("dependencies.column.child"), cell: (d) => <EndLink end={d.child} /> },
          { key: "what", header: t("dependencies.column.parent"), cell: (d) => partWords(d.parent) },
          { key: "added", header: t("dependencies.column.added"), cell: (d) => <Added dependency={d} now={now} /> },
        ];
  const key = (d: Dependency) => d.id ?? `${endWords(d.child)}\t${endWords(d.parent)}`;
  return (
    <DataTable
      label={label}
      columns={columns}
      rows={rows}
      rowKey={key}
      rowActions={(d) =>
        d.can_change &&
        d.id && (
          <RowMenu
            label={t("dependencies.actions", { what: `${partWords(d.child)} · ${endWords(d.parent)}` })}
            items={[{ label: t("dependencies.remove"), onSelect: () => setRemoving(d.id), danger: true }]}
          />
        )
      }
      rowDetail={(d) => removing !== null && removing === d.id && <Remove dependency={d} onRemoved={onChanged} onClose={() => setRemoving(null)} />}
    />
  );
}

/** What a server or device depends on, suggestions, and the form to add one. */
function Parents({ place, page, onChanged }: { place: Place; page: DependencyPage; onChanged: () => void }) {
  // A check row's "Depends on…" opens the form for that check.
  const asked = useSearchParams().get("check");
  const [adding, setAdding] = useState(asked !== null && page.can_change);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const suggest = async (parent: DependencyEnd) => {
    setBusy(parent.server_id);
    setError(null);
    try {
      await api.addDependencies({ server_id: parent.server_id! }, [{ server_id: place.id }]);
      onChanged();
    } catch (e) {
      setError(t("dependencies.form.failed", { error: describeError(e) }));
    } finally {
      setBusy(null);
    }
  };
  return (
    <section aria-label={t("dependencies.parents.title")} className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("dependencies.parents.title")}</h2>
        {page.can_change && !adding && <Button onClick={() => setAdding(true)}>{t("dependencies.parents.add")}</Button>}
      </div>
      <p className={admin.fieldNote}>{t("dependencies.parents.intro", { name: place.name })}</p>
      {page.unreachable_checks.length > 0 && (
        <Banner tone="info">
          <strong>{t("dependencies.unreachable.checks")}</strong>{" "}
          {page.unreachable_checks
            .map((c) => t("dependencies.unreachable.check", { check: c.check.name, parent: endWords(c.because) }))
            .join(" · ")}
        </Banner>
      )}
      {adding && (
        <ParentForm
          place={place}
          check={asked !== null && place.checks.includes(asked) ? asked : ""}
          onSaved={() => {
            setAdding(false);
            onChanged();
          }}
          onCancel={() => setAdding(false)}
        />
      )}
      {page.parents.length === 0 ? (
        <EmptyState>{t("dependencies.parents.empty", { name: place.name })}</EmptyState>
      ) : (
        <DependencyTable label={t("dependencies.parents.title")} rows={page.parents} side="parent" onChanged={onChanged} />
      )}
      {page.can_change && page.suggestions.length > 0 && (
        <div aria-label={t("dependencies.suggest.title")} role="group">
          <h3 className={admin.legend}>{t("dependencies.suggest.title")}</h3>
          <ul className={admin.linkList}>
            {page.suggestions.map((s) => (
              <li key={s.parent.server_id}>
                <EndLink end={s.parent} /> <span className={admin.dim}>{t(`dependencies.suggest.${s.reason}`)}</span>{" "}
                <Button size="small" disabled={busy !== null} onClick={() => void suggest(s.parent)}>
                  {busy === s.parent.server_id ? t("dependencies.suggest.adding") : t("dependencies.suggest.add")}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      )}
      {error && (
        <p className={admin.formError} role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

/** Servers, devices and relays of one customer, other than `place`. */
function useCandidates(place: Place) {
  const fetcher = useCallback(
    async (signal: AbortSignal) => {
      const [servers, relays] = await Promise.all([api.servers(signal), api.relays(signal).catch(() => ({ relays: [] as Relay[] }))]);
      const mine = (s: Server) => s.customer_id === place.customerId && !s.revoked_at && s.id !== place.id;
      return {
        servers: servers.servers.filter(mine).sort((a, b) => a.hostname.localeCompare(b.hostname)),
        relays: relays.relays
          .filter((r) => r.customer_id === place.customerId && !r.revoked_at && r.id !== place.id)
          .sort((a, b) => a.name.localeCompare(b.name)),
      };
    },
    [place.customerId, place.id],
  );
  return usePolledData(fetcher, { intervalMs: 5 * 60_000 });
}

/** The checks of a server or device, by name. */
function useChecks(serverId: string | null) {
  const fetcher = useCallback(
    async (signal: AbortSignal) => {
      if (!serverId) return [];
      const config = await api.serverSensors(serverId, signal);
      return [...new Set(config.sensors.map((s) => s.name))].sort();
    },
    [serverId],
  );
  return usePolledData(fetcher, { intervalMs: 5 * 60_000 }).data ?? [];
}

function ParentForm({
  place,
  check,
  onSaved,
  onCancel,
}: {
  place: Place;
  /** The check it starts with; `""` for all of it. */
  check: string;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const candidates = useCandidates(place).data;
  const [part, setPart] = useState(check);
  const [parent, setParent] = useState("");
  const [parentCheck, setParentCheck] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [kind, id] = parent.split(":");
  const parentChecks = useChecks(kind === "server" && id ? id : null);
  const devices = candidates?.servers.filter((s) => s.kind === "device") ?? [];
  const servers = candidates?.servers.filter((s) => s.kind !== "device") ?? [];

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!parent) {
      setError(t("dependencies.form.pickParent"));
      return;
    }
    const to: DependencyEndRequest =
      kind === "relay"
        ? { relay_id: id }
        : { server_id: id, ...(parentCheck ? { check: { name: parentCheck, instance: "" } } : {}) };
    const child: DependencyEndRequest = { server_id: place.id, ...(part ? { check: { name: part, instance: "" } } : {}) };
    setBusy(true);
    setError(null);
    try {
      await api.addDependencies(to, [child]);
      onSaved();
    } catch (e) {
      setError(t("dependencies.form.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} aria-label={t("dependencies.parents.add")} onSubmit={(e) => void submit(e)}>
      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("dependencies.form.part")}
          <select className={fieldClass} value={part} onChange={(e) => setPart(e.target.value)}>
            <option value="">{t("dependencies.form.whole", { name: place.name })}</option>
            {place.checks.map((c) => (
              <option key={c} value={c}>
                {c}
              </option>
            ))}
          </select>
        </label>
        <label className={admin.field}>
          {t("dependencies.form.parent")}
          <select
            className={fieldClass}
            value={parent}
            onChange={(e) => {
              setParent(e.target.value);
              setParentCheck("");
            }}
          >
            <option value="" disabled>
              {t("dependencies.form.choose")}
            </option>
            {devices.length > 0 && (
              <optgroup label={t("dependencies.form.group.devices")}>
                {devices.map((s) => (
                  <option key={s.id} value={`server:${s.id}`}>
                    {s.hostname}
                  </option>
                ))}
              </optgroup>
            )}
            {servers.length > 0 && (
              <optgroup label={t("dependencies.form.group.servers")}>
                {servers.map((s) => (
                  <option key={s.id} value={`server:${s.id}`}>
                    {s.hostname}
                  </option>
                ))}
              </optgroup>
            )}
            {(candidates?.relays.length ?? 0) > 0 && (
              <optgroup label={t("dependencies.form.group.relays")}>
                {candidates!.relays.map((r) => (
                  <option key={r.id} value={`relay:${r.id}`}>
                    {r.name}
                  </option>
                ))}
              </optgroup>
            )}
          </select>
        </label>
        {kind === "server" && parentChecks.length > 0 && (
          <label className={admin.field}>
            {t("dependencies.form.parentCheck")}
            <select className={fieldClass} value={parentCheck} onChange={(e) => setParentCheck(e.target.value)}>
              <option value="">{t("dependencies.form.parentWhole")}</option>
              {parentChecks.map((c) => (
                <option key={c} value={c}>
                  {c}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>
      <span className={admin.fieldNote}>{t("dependencies.form.wholeNote")}</span>
      <FormFoot busy={busy} error={error} onCancel={onCancel} />
    </form>
  );
}

function FormFoot({ busy, error, onCancel }: { busy: boolean; error: string | null; onCancel: () => void }) {
  return (
    <div className={admin.formFoot}>
      <Button type="submit" variant="k" disabled={busy}>
        {busy ? t("dependencies.form.saving") : t("dependencies.form.save")}
      </Button>
      <Button onClick={onCancel} disabled={busy}>
        {t("dependencies.form.cancel")}
      </Button>
      {error && (
        <p className={admin.formError} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** What is behind the place, and the form to put several behind it at once. */
function Children({ place, page, onChanged }: { place: Place; page: DependencyPage; onChanged: () => void }) {
  const [adding, setAdding] = useState(false);
  return (
    <section aria-label={t("dependencies.children.title")} className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("dependencies.children.title")}</h2>
        {page.can_change && !adding && <Button onClick={() => setAdding(true)}>{t("dependencies.children.add")}</Button>}
      </div>
      <p className={admin.fieldNote}>
        {place.kind === "relay" ? t("dependencies.relay.intro") : t("dependencies.children.intro", { name: place.name })}
      </p>
      {adding && (
        <ChildrenForm
          place={place}
          taken={page.children}
          onSaved={() => {
            setAdding(false);
            onChanged();
          }}
          onCancel={() => setAdding(false)}
        />
      )}
      {page.children.length === 0 ? (
        <EmptyState>{t("dependencies.children.empty", { name: place.name })}</EmptyState>
      ) : (
        <DependencyTable label={t("dependencies.children.title")} rows={page.children} side="child" onChanged={onChanged} />
      )}
      {page.behind.length > page.children.length && (
        <p className={admin.fieldNote}>{t("dependencies.children.through", { count: page.behind.length })}</p>
      )}
    </section>
  );
}

function ChildrenForm({
  place,
  taken,
  onSaved,
  onCancel,
}: {
  place: Place;
  taken: Dependency[];
  onSaved: () => void;
  onCancel: () => void;
}) {
  const candidates = useCandidates(place).data;
  const [filter, setFilter] = useState("");
  const [chosen, setChosen] = useState<string[]>([]);
  const [check, setCheck] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const already = useMemo(
    () => new Set(taken.filter((d) => !d.child.check && (d.parent.check?.name ?? "") === check).map((d) => d.child.server_id)),
    [taken, check],
  );
  const choices = (candidates?.servers ?? []).filter(
    (s) => !already.has(s.id) && s.hostname.toLowerCase().includes(filter.trim().toLowerCase()),
  );

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (chosen.length === 0) {
      setError(t("dependencies.form.pickChildren"));
      return;
    }
    const parent: DependencyEndRequest =
      place.kind === "relay"
        ? { relay_id: place.id }
        : { server_id: place.id, ...(check ? { check: { name: check, instance: "" } } : {}) };
    setBusy(true);
    setError(null);
    try {
      await api.addDependencies(
        parent,
        chosen.map((id) => ({ server_id: id })),
      );
      onSaved();
    } catch (e) {
      setError(t("dependencies.form.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} aria-label={t("dependencies.children.add")} onSubmit={(e) => void submit(e)}>
      {place.kind === "server" && place.checks.length > 0 && (
        <label className={admin.field}>
          {t("dependencies.form.behindCheck")}
          <select className={fieldClass} value={check} onChange={(e) => setCheck(e.target.value)}>
            <option value="">{t("dependencies.form.behindWhole", { name: place.name })}</option>
            {place.checks.map((c) => (
              <option key={c} value={c}>
                {t("dependencies.form.behindOne", { check: c })}
              </option>
            ))}
          </select>
        </label>
      )}
      <fieldset className={admin.fieldset}>
        <legend className={admin.legend}>{t("dependencies.form.children")}</legend>
        <input
          className={fieldClass}
          type="search"
          aria-label={t("dependencies.form.filter")}
          placeholder={t("dependencies.form.filter")}
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        {candidates && choices.length === 0 && <span className={admin.fieldNote}>{t("dependencies.form.noneLeft")}</span>}
        <div className={styles.choices}>
          {choices.map((s) => (
            <label key={s.id}>
              <input
                type="checkbox"
                checked={chosen.includes(s.id)}
                onChange={() => setChosen((c) => (c.includes(s.id) ? c.filter((x) => x !== s.id) : [...c, s.id]))}
              />
              {s.hostname}
            </label>
          ))}
        </div>
        {chosen.length > 0 && <span className={admin.fieldNote}>{t("dependencies.form.chosen", { count: chosen.length })}</span>}
      </fieldset>
      <FormFoot busy={busy} error={error} onCancel={onCancel} />
    </form>
  );
}

/**
 * On an incident's page: what it hides while its parent is down, the
 * incidents it hid, or, for one it hid, the incident that speaks for it.
 */
export function IncidentDependencies({ incident }: { incident: IncidentDetail }) {
  const hides = incident.hides ?? [];
  const hidden = incident.hidden ?? [];
  return (
    <>
      {incident.hidden_by && (
        <Banner tone="info">
          {t("dependencies.incident.hiddenBy")}{" "}
          <Link href={incidentHref(incident.hidden_by)}>{t("dependencies.incident.hiddenByLink")}</Link>
        </Banner>
      )}
      {hides.length > 0 && (
        <section aria-label={t("dependencies.incident.hides", { count: hides.length })} className={styles.section}>
          <h2 className={styles.sectionTitle}>{t("dependencies.incident.hides", { count: hides.length })}</h2>
          <p className={admin.fieldNote}>{t("dependencies.incident.hidesNote")}</p>
          <ul className={admin.linkList}>
            {hides.map((end) => (
              <li key={`${end.server_id ?? end.relay_id}\t${end.check?.name ?? ""}\t${end.check?.instance ?? ""}`}>
                <EndLink end={end} />
              </li>
            ))}
          </ul>
        </section>
      )}
      {hidden.length > 0 && (
        <p className={admin.fieldNote}>
          {t("dependencies.incident.hid", { count: hidden.length })}{" "}
          {hidden.map((id, n) => (
            <span key={id}>
              {n > 0 && ", "}
              <Link href={incidentHref(id)}>{t("dependencies.incident.hidLink", { n: n + 1 })}</Link>
            </span>
          ))}
        </p>
      )}
    </>
  );
}
