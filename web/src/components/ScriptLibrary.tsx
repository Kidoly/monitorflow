"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useState } from "react";

import { t } from "@/i18n";
import {
  type Customer,
  type LibraryScript,
  type Role,
  type ScriptDetail,
  type ScriptInterpreter,
  type ScriptParameterType,
  type ScriptRun,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { administersCustomer, administersEverything, useMe, useMultiCustomer } from "@/lib/me";
import { riskWord } from "@/lib/remediation";
import { AUTOMATION_PATH, scriptHref, scriptRunHref } from "@/lib/routes";
import {
  INTERPRETERS,
  NEW_PARAMETER,
  PARAMETER_TYPES,
  type ParameterDraft,
  RUN_ROLES,
  envName,
  parameterDraft,
  parametersOf,
  runProgress,
  runStatus,
} from "@/lib/scripts";
import { useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Page } from "@/components/ui/Page";
import { EmptyState, ErrorState, PageHeader, RefreshLine } from "@/components/ui/PageState";
import { Label, StatusLabel, type StatusValue } from "@/components/ui/StatusLabel";
import { Switch } from "@/components/ui/Switch";

import admin from "./AdminPages.module.css";
import styles from "./Scripts.module.css";

interface Draft {
  customerId: string;
  name: string;
  description: string;
  interpreter: ScriptInterpreter;
  runRole: Role;
  aiAllowed: boolean;
  timeout: string;
  body: string;
  parameters: ParameterDraft[];
  note: string;
}

function draftOf(detail: ScriptDetail | null, customerId: string): Draft {
  if (!detail) {
    return {
      customerId,
      name: "",
      description: "",
      interpreter: "sh",
      runRole: "administrator",
      aiAllowed: false,
      timeout: "600",
      body: "",
      parameters: [],
      note: "",
    };
  }
  const { script } = detail;
  return {
    customerId: script.customer_id ?? "",
    name: script.name,
    description: script.description,
    interpreter: script.interpreter,
    runRole: script.run_role,
    aiAllowed: script.ai_allowed,
    timeout: String(script.timeout_s),
    body: detail.body,
    parameters: script.parameters.map(parameterDraft),
    note: "",
  };
}

/** Words of a run's requester: the person, or the rule. */
export function requester(run: ScriptRun): string {
  if (run.rule_id) return t("scripts.runs.byRule", { name: run.rule_name ?? t("scripts.runs.deletedRule") });
  return run.requested_by_name ?? t("scripts.runs.deletedUser");
}

/** The form adding a script, or changing one (a new text or parameters make a new version). */
export function ScriptForm({
  detail,
  customers,
  defaultCustomer,
  onSaved,
  onCancel,
}: {
  detail: ScriptDetail | null;
  customers: Customer[];
  defaultCustomer: string;
  onSaved: (saved: ScriptDetail) => void;
  onCancel: () => void;
}) {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const [draft, setDraft] = useState(() => draftOf(detail, defaultCustomer));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }));
  const setParameter = (i: number, change: Partial<ParameterDraft>) =>
    set("parameters", draft.parameters.map((p, j) => (j === i ? { ...p, ...change } : p)));
  const everyCustomer = administersEverything(me);
  const ownerChoices = customers.filter((c) => administersCustomer(me, c.id));

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const parameters = parametersOf(draft.parameters);
    if (typeof parameters === "string") {
      setError(parameters);
      return;
    }
    const timeout = Number(draft.timeout);
    if (!Number.isInteger(timeout) || timeout < 10 || timeout > 3600) {
      setError(t("scripts.form.badTimeout"));
      return;
    }
    setBusy(true);
    setError(null);
    const request = {
      customer_id: draft.customerId || null,
      name: draft.name.trim(),
      description: draft.description.trim(),
      interpreter: draft.interpreter,
      run_role: draft.runRole,
      ai_allowed: draft.aiAllowed,
      timeout_s: timeout,
      body: draft.body,
      parameters,
      note: draft.note.trim(),
    };
    try {
      onSaved(detail ? await api.updateScript(detail.script.id, request) : await api.createScript(request));
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };

  const lines = Math.max(10, draft.body.split("\n").length + 1);
  return (
    <form
      className={styles.form}
      onSubmit={submit}
      aria-label={detail ? t("scripts.form.changeTitle", { name: detail.script.name }) : t("scripts.form.addTitle")}
    >
      <div className={styles.row}>
        <label className={admin.field}>
          {t("scripts.form.name")}
          <input
            className={fieldClass}
            required
            maxLength={63}
            pattern="[a-z0-9][a-z0-9_.\-]*"
            value={draft.name}
            placeholder="clear-app-cache"
            onChange={(e) => set("name", e.target.value)}
          />
        </label>
        {multiCustomer && (
          <label className={admin.field}>
            {t("scripts.form.owner")}
            <select
              className={fieldClass}
              value={draft.customerId}
              disabled={detail !== null}
              onChange={(e) => set("customerId", e.target.value)}
            >
              {(everyCustomer || detail?.script.customer_id === null) && <option value="">{t("scripts.everyCustomer")}</option>}
              {ownerChoices.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
        )}
        <label className={admin.field}>
          {t("scripts.form.interpreter")}
          <select className={fieldClass} value={draft.interpreter} onChange={(e) => set("interpreter", e.target.value as ScriptInterpreter)}>
            {INTERPRETERS.map((i) => (
              <option key={i} value={i}>
                {t(`scripts.interpreter.${i}`)}
              </option>
            ))}
          </select>
        </label>
        <label className={admin.field}>
          {t("scripts.form.runRole")}
          <select className={fieldClass} value={draft.runRole} onChange={(e) => set("runRole", e.target.value as Role)}>
            {RUN_ROLES.map((role) => (
              <option key={role} value={role}>
                {t(`common.role.${role}`)}
              </option>
            ))}
          </select>
        </label>
        <label className={admin.field}>
          {t("scripts.form.timeout")}
          <input className={fieldClass} type="number" min={10} max={3600} value={draft.timeout} onChange={(e) => set("timeout", e.target.value)} />
        </label>
      </div>
      <label className={admin.field}>
        {t("scripts.form.description")}
        <input className={fieldClass} maxLength={2000} value={draft.description} onChange={(e) => set("description", e.target.value)} />
      </label>
      <label className={admin.field}>
        {t("scripts.form.body")}
        <textarea
          className={`${fieldClass} ${styles.code}`}
          required
          spellCheck={false}
          rows={lines}
          value={draft.body}
          onChange={(e) => set("body", e.target.value)}
        />
        <span className={admin.fieldNote}>
          {draft.interpreter === "sh" ? t("scripts.form.bodyNoteSh") : t("scripts.form.bodyNotePowerShell")}
        </span>
      </label>
      <fieldset className={admin.fieldset}>
        <legend className={admin.legend}>{t("scripts.form.parameters")}</legend>
        {draft.parameters.length > 0 && (
          <div className={styles.parameters}>
            <span className={styles.head}>{t("scripts.form.parameterName")}</span>
            <span className={styles.head}>{t("scripts.form.parameterLabel")}</span>
            <span className={styles.head}>{t("scripts.form.parameterType")}</span>
            <span className={styles.head}>{t("scripts.form.parameterRequired")}</span>
            <span className={styles.head}>{t("scripts.form.parameterDefault")}</span>
            <span className={styles.head} />
            {draft.parameters.map((p, i) => (
              <ParameterRow key={i} index={i} draft={p} onChange={(c) => setParameter(i, c)} onRemove={() => set("parameters", draft.parameters.filter((_, j) => j !== i))} />
            ))}
          </div>
        )}
        <span className={admin.fieldNote}>{t("scripts.form.parametersNote")}</span>
        <div>
          <Button onClick={() => set("parameters", [...draft.parameters, { ...NEW_PARAMETER }])}>{t("scripts.form.addParameter")}</Button>
        </div>
      </fieldset>
      <Switch
        checked={draft.aiAllowed}
        onChange={(v) => set("aiAllowed", v)}
        label={t("scripts.form.aiAllowed")}
        note={t("scripts.form.aiAllowedNote")}
      />
      {detail && (
        <label className={admin.field}>
          {t("scripts.form.note")}
          <input className={fieldClass} maxLength={500} value={draft.note} placeholder={t("scripts.form.notePlaceholder")} onChange={(e) => set("note", e.target.value)} />
        </label>
      )}
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("scripts.form.saving") : detail ? t("scripts.form.save") : t("scripts.form.add")}
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

function ParameterRow({
  index,
  draft,
  onChange,
  onRemove,
}: {
  index: number;
  draft: ParameterDraft;
  onChange: (change: Partial<ParameterDraft>) => void;
  onRemove: () => void;
}) {
  const n = index + 1;
  return (
    <>
      <input
        className={fieldClass}
        aria-label={t("scripts.form.parameterNameOf", { count: n })}
        value={draft.name}
        placeholder="days"
        title={draft.name ? envName(draft.name) : undefined}
        onChange={(e) => onChange({ name: e.target.value })}
      />
      <input className={fieldClass} aria-label={t("scripts.form.parameterLabelOf", { count: n })} value={draft.label} onChange={(e) => onChange({ label: e.target.value })} />
      <select
        className={fieldClass}
        aria-label={t("scripts.form.parameterTypeOf", { count: n })}
        value={draft.type}
        onChange={(e) => onChange({ type: e.target.value as ScriptParameterType })}
      >
        {PARAMETER_TYPES.map((type) => (
          <option key={type} value={type}>
            {t(`scripts.type.${type}`)}
          </option>
        ))}
      </select>
      <Switch checked={draft.required} onChange={(v) => onChange({ required: v })} aria-label={t("scripts.form.parameterRequiredOf", { count: n })} />
      <input className={fieldClass} aria-label={t("scripts.form.parameterDefaultOf", { count: n })} value={draft.defaultValue} onChange={(e) => onChange({ defaultValue: e.target.value })} />
      <Button onClick={onRemove} aria-label={t("scripts.form.removeParameterOf", { count: n })}>
        {t("scripts.form.removeParameter")}
      </Button>
      {draft.type === "choice" && (
        <label className={`${admin.field} ${styles.choices}`}>
          {t("scripts.form.choices", { count: n })}
          <textarea
            className={fieldClass}
            rows={Math.max(2, draft.choices.split("\n").length)}
            value={draft.choices}
            onChange={(e) => onChange({ choices: e.target.value })}
          />
        </label>
      )}
    </>
  );
}

const loadScripts = (signal: AbortSignal) => api.scripts(signal);
const loadRuns = (signal: AbortSignal) => api.scriptRuns({ limit: 20 }, signal);

/** `/remediation/scripts/`: the library, a way to add to it, and the latest runs. */
export function ScriptLibrary() {
  const me = useMe();
  const scope = useScope();
  const multiCustomer = useMultiCustomer();
  const scripts = usePolledData(loadScripts, { intervalMs: 60_000 });
  const runs = usePolledData(loadRuns);
  const [adding, setAdding] = useState(false);
  const [customers, setCustomers] = useState<Customer[] | null>(null);
  const canAdd = administersCustomer(me);
  const open = useCallback(() => {
    setAdding(true);
    if (!customers) void api.customers().then((list) => setCustomers(list.customers), () => setCustomers([]));
  }, [customers]);
  const shown = (scripts.data?.scripts ?? []).filter((s) => !scope.customerId || s.customer_id === null || s.customer_id === scope.customerId);
  const customerName = (id: string | null) =>
    id === null ? t("scripts.everyCustomer") : (customers?.find((c) => c.id === id)?.name ?? t("scripts.oneCustomer"));

  const columns: Column<LibraryScript>[] = [
    {
      key: "name",
      header: t("scripts.column.script"),
      cell: (s) => <PrimaryCell title={s.name} href={scriptHref(s.id)} sub={s.description || undefined} />,
      className: admin.wrap,
    },
    { key: "interpreter", header: t("scripts.column.interpreter"), cell: (s) => t(`scripts.interpreter.${s.interpreter}`) },
    { key: "role", header: t("scripts.column.runRole"), cell: (s) => t(`common.role.${s.run_role}`) },
    { key: "version", header: t("scripts.column.version"), cell: (s) => t("scripts.versionNumber", { version: s.version }) },
    {
      key: "ai",
      header: t("scripts.column.ai"),
      cell: (s) => (s.ai_allowed ? <Label tone="info">{t("scripts.aiYes")}</Label> : <span className={styles.muted}>{t("scripts.aiNo")}</span>),
    },
    ...(multiCustomer ? [{ key: "owner", header: t("scripts.column.owner"), cell: (s: LibraryScript) => customerName(s.customer_id) }] : []),
  ];

  return (
    <Page>
      <PageHeader
        title={t("scripts.title")}
        meta={<RefreshLine updatedAt={scripts.updatedAt} stale={scripts.stale} intervalMs={60_000} />}
        actions={
          <>
            <ButtonLink href={AUTOMATION_PATH}>{t("scripts.automationLink")}</ButtonLink>
            {canAdd && !adding && (
              <Button variant="k" onClick={open}>
                {t("scripts.add")}
              </Button>
            )}
          </>
        }
      />
      <p className={admin.footnote}>{t("scripts.intro")}</p>
      {adding && (
        <section className={admin.section} aria-label={t("scripts.form.addTitle")}>
          <div className={admin.sectionHead}>
            <h2 className={admin.sectionTitle}>{t("scripts.form.addTitle")}</h2>
          </div>
          {customers ? (
            <ScriptForm
              detail={null}
              customers={customers}
              defaultCustomer={administersEverything(me) ? "" : (scope.customerId ?? customers.find((c) => administersCustomer(me, c.id))?.id ?? "")}
              onSaved={(saved) => {
                setAdding(false);
                scripts.refresh();
                window.location.assign(scriptHref(saved.script.id));
              }}
              onCancel={() => setAdding(false)}
            />
          ) : (
            <p className={admin.sectionNote}>{t("scripts.loading")}</p>
          )}
        </section>
      )}
      {!scripts.data && scripts.error !== null && !isUnauthorized(scripts.error) ? (
        <ErrorState what={t("scripts.what")} error={describeError(scripts.error)} onRetry={scripts.refresh} />
      ) : scripts.data && shown.length === 0 ? (
        <EmptyState title={t("scripts.emptyTitle")} actions={canAdd && !adding ? <Button variant="k" onClick={open}>{t("scripts.add")}</Button> : undefined}>
          {t("scripts.empty")}
        </EmptyState>
      ) : (
        <DataTable label={t("scripts.title")} columns={columns} rows={shown} rowKey={(s) => s.id} loading={!scripts.data} />
      )}
      <RunsTable title={t("scripts.runs.latest")} runs={runs.data?.runs ?? null} />
    </Page>
  );
}

/** A table of runs, newest first, each linking to its page. */
export function RunsTable({ title, runs, empty }: { title: string; runs: ScriptRun[] | null; empty?: string }) {
  if (runs && runs.length === 0 && !empty) return null;
  const columns: Column<ScriptRun>[] = [
    {
      key: "run",
      header: t("scripts.runs.run"),
      cell: (r) => (
        <PrimaryCell
          title={t("scripts.runs.of", { name: r.script_name, version: r.version })}
          href={scriptRunHref(r.id)}
          sub={requester(r)}
        />
      ),
    },
    {
      key: "state",
      header: t("scripts.runs.state"),
      cell: (r) => <StatusLabel kind="scriptRun" value={runStatus(r) as StatusValue<"scriptRun">} />,
    },
    { key: "tier", header: t("scripts.runs.tier"), cell: (r) => <StatusLabel kind="risk" value={riskWord(r.tier)} /> },
    { key: "servers", header: t("scripts.runs.servers.header"), cell: (r) => runProgress(r) },
    {
      key: "when",
      header: t("scripts.runs.when"),
      cell: (r) => formatDateTime(r.dispatched_at ?? r.scheduled_at),
    },
  ];
  return (
    <section className={admin.section} aria-label={title}>
      <div className={admin.sectionHead}>
        <h2 className={admin.sectionTitle}>{title}</h2>
      </div>
      {runs && runs.length === 0 ? (
        <p className={admin.sectionNote}>{empty}</p>
      ) : (
        <DataTable bare label={title} columns={columns} rows={runs ?? []} rowKey={(r) => r.id} loading={runs === null} />
      )}
    </section>
  );
}

/** A link to a script by name, for other pages. */
export function ScriptLink({ id, name }: { id: string; name: string }) {
  return <Link href={scriptHref(id)}>{name}</Link>;
}
