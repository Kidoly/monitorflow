"use client";

import { useRouter, useSearchParams } from "next/navigation";
import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type BusinessService,
  type Customer,
  type ScriptDetail,
  type ScriptParameter,
  type ScriptRun,
  type Server,
  type Site,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { formatDateTime } from "@/lib/format";
import { useMe, useMultiCustomer } from "@/lib/me";
import { SCRIPTS_PATH, scriptRunHref } from "@/lib/routes";
import { type TargetKind, envName, scheduledAt, targetOf, valuesOf } from "@/lib/scripts";
import { usePolling } from "@/lib/usePolling";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { FactsBox } from "@/components/ui/FactsBox";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { Banner, LoadingBlock, NotFound, PageHeader } from "@/components/ui/PageState";
import { SegmentedControl } from "@/components/ui/SegmentedControl";
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import { RunsTable, ScriptForm } from "./ScriptLibrary";
import styles from "./Scripts.module.css";

const REFRESH_MS = 15_000;

/** `/remediation/scripts/view/?id=…` (ADR-0020 routing). */
export function ScriptView() {
  const id = useSearchParams().get("id");
  return <ScriptPage key={id} id={id} />;
}

export function ScriptPage({ id }: { id: string | null }) {
  const valid = id !== null && UUID.test(id);
  const router = useRouter();
  /** Undefined until loaded, null when not found. */
  const [detail, setDetail] = useState<ScriptDetail | null | undefined>(undefined);
  const [runs, setRuns] = useState<ScriptRun[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<"run" | "edit" | null>(null);
  const [customers, setCustomers] = useState<Customer[]>([]);
  const load = useCallback(
    async (signal?: AbortSignal) => {
      try {
        const [script, list] = await Promise.all([
          api.script(id ?? "", signal),
          api.scriptRuns({ script_id: id ?? "", limit: 50 }, signal),
        ]);
        setDetail(script);
        setRuns(list.runs);
        setError(null);
      } catch (e) {
        if (isAbortError(e)) return;
        if (e instanceof ApiError && e.status === 404) setDetail(null);
        else setError(describeError(e));
      }
    },
    [id],
  );
  usePolling(load, REFRESH_MS, valid);
  useEffect(() => {
    if (detail) document.title = `${detail.script.name} · MonitorFlow`;
  }, [detail]);
  useEffect(() => {
    if (mode === "edit" && customers.length === 0) void api.customers().then((l) => setCustomers(l.customers), () => {});
  }, [mode, customers.length]);

  if (!valid || detail === null) {
    return (
      <NotFound title={t("scripts.notFound")} actions={<ButtonLink href={SCRIPTS_PATH}>{t("scripts.back")}</ButtonLink>}>
        {t("scripts.notFoundText")}
      </NotFound>
    );
  }
  if (!detail) return error ? <Banner tone="bad">{error}</Banner> : <LoadingBlock label={t("scripts.loading")} />;
  const { script } = detail;
  const deleted = script.deleted_at !== null;
  const facts = [
    { label: t("scripts.fact.interpreter"), value: t(`scripts.interpreter.${script.interpreter}`) },
    { label: t("scripts.fact.runRole"), value: t(`common.role.${script.run_role}`) },
    { label: t("scripts.fact.timeout"), value: t("scripts.seconds", { count: script.timeout_s }) },
    { label: t("scripts.fact.ai"), value: script.ai_allowed ? t("scripts.fact.aiYes") : t("scripts.fact.aiNo") },
    { label: t("scripts.fact.version"), value: t("scripts.versionNumber", { version: script.version }) },
    {
      label: t("scripts.fact.parameters"),
      value: script.parameters.length
        ? script.parameters.map((p) => `${p.name} (${envName(p.name)})`).join(", ")
        : t("scripts.fact.noParameters"),
    },
    { label: t("scripts.fact.updated"), value: formatDateTime(script.updated_at) },
  ];
  const versionColumns: Column<ScriptDetail["versions"][number]>[] = [
    { key: "version", header: t("scripts.column.version"), cell: (v) => t("scripts.versionNumber", { version: v.version }) },
    { key: "note", header: t("scripts.versions.note"), cell: (v) => v.note || t("scripts.versions.noNote"), className: admin.wrap },
    { key: "by", header: t("scripts.versions.by"), cell: (v) => v.created_by_name ?? t("scripts.runs.deletedUser") },
    { key: "at", header: t("scripts.versions.at"), cell: (v) => formatDateTime(v.created_at) },
  ];

  return (
    <Page>
      <Breadcrumb parents={[{ label: t("scripts.title"), href: SCRIPTS_PATH }]} />
      <PageHeader
        detail
        title={script.name}
        status={deleted ? <Label tone="neutral">{t("scripts.deleted")}</Label> : undefined}
        reason={script.description || undefined}
        actions={
          !deleted && (
            <>
              <Button variant="k" onClick={() => setMode(mode === "run" ? null : "run")} aria-expanded={mode === "run"}>
                {t("scripts.run.open")}
              </Button>
              {script.can_manage && (
                <Button onClick={() => setMode(mode === "edit" ? null : "edit")} aria-expanded={mode === "edit"}>
                  {t("scripts.edit")}
                </Button>
              )}
              {script.can_manage && (
                <InlineConfirm
                  trigger={t("scripts.delete")}
                  title={t("scripts.deleteTitle", { name: script.name })}
                  consequence={t("scripts.deleteConsequence")}
                  confirmLabel={t("scripts.deleteConfirm", { name: script.name })}
                  describeError={describeError}
                  onConfirm={async () => {
                    await api.deleteScript(script.id);
                    router.push(SCRIPTS_PATH);
                  }}
                />
              )}
            </>
          )
        }
      />
      {error && <Banner tone="bad">{error}</Banner>}
      {mode === "run" && (
        <RunForm
          script={detail}
          onStarted={(runId) => {
            setMode(null);
            router.push(scriptRunHref(runId));
          }}
          onCancel={() => setMode(null)}
        />
      )}
      {mode === "edit" && (
        <section className={admin.section} aria-label={t("scripts.form.changeTitle", { name: script.name })}>
          <div className={admin.sectionHead}>
            <h2 className={admin.sectionTitle}>{t("scripts.form.changeTitle", { name: script.name })}</h2>
          </div>
          <ScriptForm
            detail={detail}
            customers={customers}
            defaultCustomer={script.customer_id ?? ""}
            onSaved={(saved) => {
              setDetail(saved);
              setMode(null);
            }}
            onCancel={() => setMode(null)}
          />
        </section>
      )}
      <FactsBox facts={facts} />
      <section className={admin.section} aria-label={t("scripts.bodyTitle")}>
        <div className={admin.sectionHead}>
          <h2 className={admin.sectionTitle}>{t("scripts.bodyTitle")}</h2>
        </div>
        <div className={styles.sectionBody}>
          <pre className={styles.pre}>{detail.body}</pre>
        </div>
      </section>
      <RunsTable title={t("scripts.runs.title")} runs={runs} empty={t("scripts.runs.none")} />
      <section className={admin.section} aria-label={t("scripts.versions.title")}>
        <div className={admin.sectionHead}>
          <h2 className={admin.sectionTitle}>{t("scripts.versions.title")}</h2>
        </div>
        <DataTable
          bare
          label={t("scripts.versions.title")}
          columns={versionColumns}
          rows={detail.versions}
          rowKey={(v) => String(v.version)}
          detailInset
          rowDetail={(v) =>
            v.version !== script.version ? (
              <details>
                <summary>{t("scripts.versions.show", { version: v.version })}</summary>
                <pre className={styles.pre}>{v.body}</pre>
              </details>
            ) : null
          }
        />
      </section>
    </Page>
  );
}

/** One input per parameter, by its type. */
function ValueField({ parameter, value, onChange }: { parameter: ScriptParameter; value: string; onChange: (v: string) => void }) {
  const label = parameter.label || parameter.name;
  const placeholder = parameter.default ?? undefined;
  const control =
    parameter.type === "choice" || parameter.type === "boolean" ? (
      <select className={fieldClass} value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{parameter.default ? t("scripts.run.default", { value: parameter.default }) : t("scripts.run.choose")}</option>
        {(parameter.type === "boolean" ? ["true", "false"] : parameter.choices).map((c) => (
          <option key={c} value={c}>
            {parameter.type === "boolean" ? t(c === "true" ? "scripts.run.yes" : "scripts.run.no") : c}
          </option>
        ))}
      </select>
    ) : (
      <input
        className={fieldClass}
        inputMode={parameter.type === "integer" ? "numeric" : undefined}
        value={value}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
    );
  return (
    <label className={admin.field}>
      {parameter.required && parameter.default === null ? t("scripts.run.requiredLabel", { label }) : label}
      {control}
      <span className={admin.fieldNote}>{envName(parameter.name)}</span>
    </label>
  );
}

interface Places {
  servers: Server[];
  sites: Site[];
  services: BusinessService[];
}

/** Run now or at a time, on servers, a site or a business service. */
function RunForm({ script, onStarted, onCancel }: { script: ScriptDetail; onStarted: (runId: string) => void; onCancel: () => void }) {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const [places, setPlaces] = useState<Places | null>(null);
  const [kind, setKind] = useState<TargetKind>("servers");
  const [servers, setServers] = useState<string[]>([]);
  const [site, setSite] = useState("");
  const [service, setService] = useState("");
  const [values, setValues] = useState<Record<string, string>>({});
  const [version, setVersion] = useState(String(script.script.version));
  const [when, setWhen] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const owner = script.script.customer_id;
  const windows = script.script.interpreter === "powershell";
  useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      api.servers(controller.signal),
      api.sites(controller.signal),
      api.businessServices(controller.signal).catch(() => ({ services: [] })),
    ])
      .then(([s, si, bs]) => setPlaces({ servers: s.servers, sites: si.sites, services: bs.services }))
      .catch((e) => !isAbortError(e) && setPlaces({ servers: [], sites: [], services: [] }));
    return () => controller.abort();
  }, []);
  const fits = (s: Server) => (windows ? s.os_family === "windows" : s.os_family !== "windows");
  const ofOwner = (customerId: string) => !owner || owner === customerId;
  const serverChoices = (places?.servers ?? []).filter((s) => fits(s) && ofOwner(s.customer_id) && s.revoked_at === null);
  const selectedVersion = script.versions.find((v) => String(v.version) === version) ?? script.versions[0];
  const parameters = selectedVersion?.parameters ?? script.script.parameters;

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const target = targetOf(kind, { servers, site, service });
    if (typeof target === "string") return setError(target);
    const given = valuesOf(parameters, values);
    if (typeof given === "string") return setError(given);
    setBusy(true);
    setError(null);
    try {
      const run = await api.runScript(script.script.id, {
        target,
        parameters: given,
        version: Number(version),
        scheduled_at: scheduledAt(when),
      });
      onStarted(run.run.id);
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={admin.section} aria-label={t("scripts.run.title", { name: script.script.name })}>
      <div className={admin.sectionHead}>
        <h2 className={admin.sectionTitle}>{t("scripts.run.title", { name: script.script.name })}</h2>
      </div>
      {!places ? (
        <p className={admin.sectionNote}>{t("scripts.loading")}</p>
      ) : (
        <form className={styles.form} onSubmit={submit}>
          <SegmentedControl
            label={t("scripts.run.on")}
            options={[
              { value: "servers", label: t("scripts.run.onServers") },
              { value: "site", label: t("scripts.run.onSite") },
              { value: "business_service", label: t("scripts.run.onService") },
            ]}
            value={kind}
            onChange={(v) => setKind(v as TargetKind)}
          />
          {kind === "servers" && (
            <fieldset className={admin.fieldset}>
              <legend className={admin.legend}>{t("scripts.run.servers")}</legend>
              {serverChoices.length === 0 ? (
                <p className={admin.fieldNote}>{windows ? t("scripts.run.noWindows") : t("scripts.run.noLinux")}</p>
              ) : (
                <div className={styles.serverList}>
                  {serverChoices.map((s) => (
                    <label key={s.id}>
                      <input
                        type="checkbox"
                        checked={servers.includes(s.id)}
                        onChange={(e) => setServers(e.target.checked ? [...servers, s.id] : servers.filter((x) => x !== s.id))}
                      />
                      {multiCustomer ? `${s.hostname} (${s.customer_name}, ${s.site_name})` : s.hostname}
                    </label>
                  ))}
                </div>
              )}
            </fieldset>
          )}
          {kind === "site" && (
            <label className={admin.field}>
              {t("scripts.run.site")}
              <select className={fieldClass} value={site} onChange={(e) => setSite(e.target.value)}>
                <option value="">{t("scripts.run.choose")}</option>
                {places.sites
                  .filter((s) => ofOwner(s.customer_id))
                  .map((s) => (
                    <option key={s.id} value={s.id}>
                      {multiCustomer ? `${s.customer_name} · ${s.name}` : s.name}
                    </option>
                  ))}
              </select>
            </label>
          )}
          {kind === "business_service" && (
            <label className={admin.field}>
              {t("scripts.run.service")}
              <select className={fieldClass} value={service} onChange={(e) => setService(e.target.value)}>
                <option value="">{t("scripts.run.choose")}</option>
                {places.services
                  .filter((s) => ofOwner(s.customer_id))
                  .map((s) => (
                    <option key={s.id} value={s.id}>
                      {s.name}
                    </option>
                  ))}
              </select>
            </label>
          )}
          {parameters.length > 0 && (
            <div className={styles.row}>
              {parameters.map((p) => (
                <ValueField key={p.name} parameter={p} value={values[p.name] ?? ""} onChange={(v) => setValues({ ...values, [p.name]: v })} />
              ))}
            </div>
          )}
          <div className={styles.row}>
            <label className={admin.field}>
              {t("scripts.run.version")}
              <select className={fieldClass} value={version} onChange={(e) => setVersion(e.target.value)}>
                {script.versions.map((v) => (
                  <option key={v.version} value={v.version}>
                    {v.version === script.script.version ? t("scripts.run.current", { version: v.version }) : t("scripts.versionNumber", { version: v.version })}
                  </option>
                ))}
              </select>
            </label>
            <label className={admin.field}>
              {t("scripts.run.when")}
              <input className={fieldClass} type="datetime-local" value={when} onChange={(e) => setWhen(e.target.value)} />
            </label>
          </div>
          <p className={admin.fieldNote}>{me ? t("scripts.run.approvalNote") : null}</p>
          <div className={admin.formFoot}>
            <Button type="submit" variant="k" disabled={busy}>
              {busy ? t("scripts.run.starting") : when ? t("scripts.run.schedule") : t("scripts.run.now")}
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
      )}
    </section>
  );
}
