"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useState } from "react";

import { t } from "@/i18n";
import {
  type AutomationEvent,
  type AutomationFiring,
  type AutomationRule,
  type Customer,
  type LibraryScript,
  type Site,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { administersCustomer, administersEverything, administersSite, useMe, useMultiCustomer } from "@/lib/me";
import { SCRIPTS_PATH, scriptHref, scriptRunHref } from "@/lib/routes";
import { EVENTS, valuesOf } from "@/lib/scripts";
import { usePolledData } from "@/lib/usePolledData";
import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, PageHeader, RefreshLine } from "@/components/ui/PageState";
import { RowMenu } from "@/components/ui/RowMenu";
import { Label } from "@/components/ui/StatusLabel";
import { Switch } from "@/components/ui/Switch";

import admin from "./AdminPages.module.css";
import styles from "./Scripts.module.css";

interface Places {
  scripts: LibraryScript[];
  customers: Customer[];
  sites: Site[];
}

interface Draft {
  name: string;
  /** "" for everything, `c:<id>` for a customer, `s:<id>` for a site. */
  place: string;
  event: AutomationEvent;
  metric: string;
  scriptId: string;
  values: Record<string, string>;
  enabled: boolean;
  cooldown: string;
}

function draftOf(rule: AutomationRule | null): Draft {
  if (!rule) return { name: "", place: "", event: "incident_opened", metric: "", scriptId: "", values: {}, enabled: true, cooldown: "60" };
  return {
    name: rule.name,
    place: rule.site_id ? `s:${rule.site_id}` : rule.customer_id ? `c:${rule.customer_id}` : "",
    event: rule.event,
    metric: rule.metric ?? "",
    scriptId: rule.script_id,
    values: rule.parameters,
    enabled: rule.enabled,
    cooldown: String(rule.cooldown_minutes),
  };
}

/** The rule's trigger in words. */
export function describeEvent(rule: Pick<AutomationRule, "event" | "metric">): string {
  switch (rule.event) {
    case "incident_opened":
      return rule.metric ? t("scripts.rules.when.incidentOf", { metric: rule.metric }) : t("scripts.rules.when.anyIncident");
    case "check_failed":
      return t("scripts.rules.when.check", { metric: rule.metric ?? "" });
    case "server_joined":
      return t("scripts.rules.when.joined");
  }
}

function RuleForm({ rule, places, onSaved, onCancel }: { rule: AutomationRule | null; places: Places; onSaved: () => void; onCancel: () => void }) {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const [draft, setDraft] = useState(() => draftOf(rule));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }));
  const placeCustomer = draft.place.startsWith("c:")
    ? draft.place.slice(2)
    : draft.place.startsWith("s:")
      ? places.sites.find((s) => s.id === draft.place.slice(2))?.customer_id
      : undefined;
  const scripts = places.scripts.filter((s) => s.customer_id === null || s.customer_id === placeCustomer);
  const script = scripts.find((s) => s.id === draft.scriptId);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!script) return setError(t("scripts.rules.form.noScript"));
    const values = valuesOf(script.parameters, draft.values);
    if (typeof values === "string") return setError(values);
    setBusy(true);
    setError(null);
    const request = {
      name: draft.name.trim(),
      customer_id: draft.place.startsWith("c:") ? draft.place.slice(2) : null,
      site_id: draft.place.startsWith("s:") ? draft.place.slice(2) : null,
      event: draft.event,
      metric: draft.event === "server_joined" ? null : draft.metric.trim() || null,
      script_id: script.id,
      parameters: values,
      enabled: draft.enabled,
      cooldown_minutes: Number(draft.cooldown) || 60,
    };
    try {
      if (rule) await api.updateAutomationRule(rule.id, request);
      else await api.createAutomationRule(request);
      onSaved();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className={styles.form} onSubmit={submit} aria-label={rule ? t("scripts.rules.form.changeTitle", { name: rule.name }) : t("scripts.rules.form.addTitle")}>
      <div className={styles.row}>
        <label className={admin.field}>
          {t("scripts.rules.form.name")}
          <input className={fieldClass} required maxLength={80} value={draft.name} onChange={(e) => set("name", e.target.value)} />
        </label>
        <label className={admin.field}>
          {t("scripts.rules.form.place")}
          <select className={fieldClass} value={draft.place} onChange={(e) => setDraft({ ...draft, place: e.target.value, scriptId: "", values: {} })}>
            {administersEverything(me) && <option value="">{t("scripts.rules.everywhere")}</option>}
            {places.customers
              .filter((c) => administersCustomer(me, c.id))
              .map((c) => (
                <option key={c.id} value={`c:${c.id}`}>
                  {multiCustomer ? t("scripts.rules.customer", { name: c.name }) : t("scripts.rules.allSites")}
                </option>
              ))}
            {places.sites
              .filter((s) => administersSite(me, s.customer_id, s.id))
              .map((s) => (
                <option key={s.id} value={`s:${s.id}`}>
                  {multiCustomer ? `${s.customer_name} · ${s.name}` : s.name}
                </option>
              ))}
          </select>
        </label>
      </div>
      <div className={styles.row}>
        <label className={admin.field}>
          {t("scripts.rules.form.event")}
          <select className={fieldClass} value={draft.event} onChange={(e) => set("event", e.target.value as AutomationEvent)}>
            {EVENTS.map((e) => (
              <option key={e} value={e}>
                {t(`scripts.rules.event.${e}`)}
              </option>
            ))}
          </select>
        </label>
        {draft.event !== "server_joined" && (
          <label className={admin.field}>
            {draft.event === "check_failed" ? t("scripts.rules.form.check") : t("scripts.rules.form.metric")}
            <input
              className={fieldClass}
              required={draft.event === "check_failed"}
              value={draft.metric}
              placeholder="monitorflow_disk_used_ratio"
              onChange={(e) => set("metric", e.target.value)}
            />
          </label>
        )}
        <label className={admin.field}>
          {t("scripts.rules.form.cooldown")}
          <input className={fieldClass} type="number" min={1} max={10080} value={draft.cooldown} onChange={(e) => set("cooldown", e.target.value)} />
        </label>
      </div>
      <label className={admin.field}>
        {t("scripts.rules.form.script")}
        <select className={fieldClass} required value={draft.scriptId} onChange={(e) => setDraft({ ...draft, scriptId: e.target.value, values: {} })}>
          <option value="">{t("scripts.run.choose")}</option>
          {scripts.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name} ({t(`scripts.interpreter.${s.interpreter}`)})
            </option>
          ))}
        </select>
      </label>
      {script && script.parameters.length > 0 && (
        <div className={styles.row}>
          {script.parameters.map((p) => (
            <label key={p.name} className={admin.field}>
              {p.label || p.name}
              <input
                className={fieldClass}
                value={draft.values[p.name] ?? ""}
                placeholder={p.default ?? undefined}
                onChange={(e) => set("values", { ...draft.values, [p.name]: e.target.value })}
              />
            </label>
          ))}
        </div>
      )}
      <Switch checked={draft.enabled} onChange={(v) => set("enabled", v)} label={t("scripts.rules.form.enabled")} />
      <p className={admin.fieldNote}>{t("scripts.rules.form.note")}</p>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("scripts.form.saving") : rule ? t("scripts.form.save") : t("scripts.rules.form.add")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("scripts.form.cancel")}
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

function Firings({ rule }: { rule: AutomationRule }) {
  const fetcher = useCallback((signal: AbortSignal) => api.automationRule(rule.id, signal), [rule.id]);
  const { data } = usePolledData(fetcher);
  if (!data) return <p className={styles.muted}>{t("scripts.loading")}</p>;
  if (data.firings.length === 0) return <p className={styles.muted}>{t("scripts.rules.noFirings")}</p>;
  const columns: Column<AutomationFiring>[] = [
    { key: "at", header: t("scripts.rules.firing.at"), cell: (f) => formatDateTime(f.created_at) },
    { key: "server", header: t("scripts.rules.firing.server"), cell: (f) => f.hostname ?? t("scripts.rules.firing.gone") },
    {
      key: "result",
      header: t("scripts.rules.firing.result"),
      cell: (f) => (f.run_id ? <Link href={scriptRunHref(f.run_id)}>{t("scripts.rules.firing.run")}</Link> : f.skipped),
      className: admin.wrap,
    },
  ];
  return <DataTable label={t("scripts.rules.firings", { name: rule.name })} columns={columns} rows={data.firings} rowKey={(f) => f.event_id} />;
}

const loadRules = (signal: AbortSignal) => api.automationRules(signal);

/** `/remediation/automation/`: rules that run a library script when something happens. */
export function AutomationRules() {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const { data, error, refresh, updatedAt, stale } = usePolledData(loadRules, { intervalMs: 60_000 });
  const [editing, setEditing] = useState<AutomationRule | "new" | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [showing, setShowing] = useState<string | null>(null);
  const [places, setPlaces] = useState<Places | null>(null);
  const canAdd = me?.roles.some((g) => g.role === "administrator") ?? false;
  const open = (target: AutomationRule | "new") => {
    setEditing(target);
    if (!places)
      void Promise.all([api.scripts(), api.customers(), api.sites()])
        .then(([s, c, si]) => setPlaces({ scripts: s.scripts, customers: c.customers, sites: si.sites }))
        .catch(() => setPlaces({ scripts: [], customers: [], sites: [] }));
  };
  const saved = () => {
    setEditing(null);
    refresh();
  };
  const placeName = (r: AutomationRule) =>
    r.site_id ? t("scripts.rules.oneSite") : r.customer_id ? (multiCustomer ? t("scripts.rules.oneCustomer") : t("scripts.rules.allSites")) : t("scripts.rules.everywhere");
  const columns: Column<AutomationRule>[] = [
    { key: "name", header: t("scripts.rules.column.rule"), cell: (r) => <PrimaryCell title={r.name} sub={describeEvent(r)} /> },
    { key: "script", header: t("scripts.rules.column.script"), cell: (r) => <Link href={scriptHref(r.script_id)}>{r.script_name}</Link> },
    { key: "place", header: t("scripts.rules.column.place"), cell: placeName },
    {
      key: "state",
      header: t("scripts.rules.column.state"),
      cell: (r) => (r.enabled ? <Label tone="ok">{t("scripts.rules.on")}</Label> : <Label tone="neutral">{t("scripts.rules.paused")}</Label>),
    },
    { key: "cooldown", header: t("scripts.rules.column.cooldown"), cell: (r) => t("scripts.minutes", { count: r.cooldown_minutes }) },
  ];
  return (
    <Page>
      <PageHeader
        title={t("scripts.rules.title")}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} intervalMs={60_000} />}
        actions={
          <>
            <ButtonLink href={SCRIPTS_PATH}>{t("scripts.rules.toLibrary")}</ButtonLink>
            {canAdd && editing === null && (
              <Button variant="k" onClick={() => open("new")}>
                {t("scripts.rules.add")}
              </Button>
            )}
          </>
        }
      />
      <p className={admin.footnote}>{t("scripts.rules.intro")}</p>
      {editing !== null && (
        <section className={admin.section} aria-label={editing === "new" ? t("scripts.rules.form.addTitle") : t("scripts.rules.form.changeTitle", { name: editing.name })}>
          <div className={admin.sectionHead}>
            <h2 className={admin.sectionTitle}>
              {editing === "new" ? t("scripts.rules.form.addTitle") : t("scripts.rules.form.changeTitle", { name: editing.name })}
            </h2>
          </div>
          {places ? (
            <RuleForm rule={editing === "new" ? null : editing} places={places} onSaved={saved} onCancel={() => setEditing(null)} />
          ) : (
            <p className={admin.sectionNote}>{t("scripts.loading")}</p>
          )}
        </section>
      )}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("scripts.rules.what")} error={describeError(error)} onRetry={refresh} />
      ) : data && data.rules.length === 0 ? (
        <EmptyState title={t("scripts.rules.emptyTitle")} actions={canAdd && editing === null ? <Button variant="k" onClick={() => open("new")}>{t("scripts.rules.add")}</Button> : undefined}>
          {t("scripts.rules.empty")}
        </EmptyState>
      ) : (
        <DataTable
          label={t("scripts.rules.title")}
          columns={columns}
          rows={data?.rules ?? []}
          rowKey={(r) => r.id}
          loading={!data}
          rowActions={(r) => (
            <RowMenu
              label={t("scripts.rules.actions", { name: r.name })}
              items={[
                { label: showing === r.id ? t("scripts.rules.hideFirings") : t("scripts.rules.showFirings"), onSelect: () => setShowing(showing === r.id ? null : r.id) },
                ...(r.can_manage
                  ? [
                      { label: t("scripts.rules.change"), onSelect: () => open(r) },
                      { label: t("scripts.rules.remove"), onSelect: () => setRemoving(r.id), danger: true },
                    ]
                  : []),
              ]}
            />
          )}
          rowDetail={(r) =>
            removing === r.id ? (
              <InlineConfirm
                layout="row"
                trigger={t("scripts.rules.remove")}
                title={t("scripts.rules.removeTitle", { name: r.name })}
                consequence={t("scripts.rules.removeConsequence")}
                confirmLabel={t("scripts.rules.remove")}
                describeError={describeError}
                onClose={() => setRemoving(null)}
                onConfirm={async () => {
                  await api.deleteAutomationRule(r.id);
                  setRemoving(null);
                  refresh();
                }}
              />
            ) : showing === r.id ? (
              <Firings rule={r} />
            ) : null
          }
        />
      )}
    </Page>
  );
}
