"use client";

import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import { type BusinessService, type BusinessServiceRequest, type Customer, type Server, api, describeError, isUnauthorized } from "@/lib/api";
import { modifiesCustomer, useMe, useMultiCustomer } from "@/lib/me";
import { formatTarget } from "@/lib/reports";
import { useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { RowMenu } from "@/components/ui/RowMenu";
import { EmptyState, ErrorState } from "@/components/ui/PageState";
import { SegmentedControl } from "@/components/ui/SegmentedControl";

import admin from "./AdminPages.module.css";
import styles from "./Reports.module.css";

/** A member as the form holds it: a server, and a check of it or "" for all of it. */
interface MemberDraft {
  serverId: string;
  check: string;
}

interface Draft {
  customerId: string;
  name: string;
  description: string;
  atLeast: boolean;
  count: number;
  target: string;
  members: MemberDraft[];
}

const checkKey = (name: string, instance: string) => `${name}\t${instance}`;

function draftOf(service: BusinessService | null, customerId: string): Draft {
  if (!service) {
    return { customerId, name: "", description: "", atLeast: false, count: 1, target: "", members: [{ serverId: "", check: "" }] };
  }
  return {
    customerId: service.customer_id,
    name: service.name,
    description: service.description,
    atLeast: service.rule === "at_least",
    count: service.at_least ?? 1,
    target: service.target === null ? "" : String(service.target),
    members: service.members.map((m) => ({ serverId: m.server_id, check: m.check ? checkKey(m.check.name, m.check.instance) : "" })),
  };
}

/** The draft as the API takes it, or why not. */
export function serviceRequestOf(draft: Draft, multiCustomer: boolean): BusinessServiceRequest | string {
  const members = draft.members.filter((m) => m.serverId);
  if (!draft.name.trim()) return t("reports.service.noName");
  if (members.length === 0) return t("reports.service.noMember");
  let target: number | null = null;
  if (draft.target.trim()) {
    target = Number(draft.target.trim().replace(",", "."));
    if (!Number.isFinite(target) || target < 50 || target > 100) return t("reports.service.badTarget");
  }
  if (draft.atLeast && (draft.count < 1 || draft.count > members.length)) return t("reports.service.badCount", { count: members.length });
  return {
    ...(multiCustomer && { customer_id: draft.customerId }),
    name: draft.name.trim(),
    description: draft.description.trim(),
    rule: draft.atLeast ? "at_least" : "all",
    at_least: draft.atLeast ? draft.count : null,
    target,
    members: members.map((m) => {
      const [name = "", instance = ""] = m.check.split("\t");
      return { server_id: m.serverId, check: m.check ? { name, instance } : null };
    }),
  };
}

/** The rule in words: "All of them", "At least 2 of 3". */
export function describeRule(service: Pick<BusinessService, "rule" | "at_least" | "members">): string {
  return service.rule === "at_least" && service.at_least
    ? t("reports.service.ruleAtLeast", { count: service.at_least, total: service.members.length })
    : t("reports.service.ruleAll");
}

/** The checks of one server for a member's picker. */
function CheckPicker({ serverId, value, onChange, label }: { serverId: string; value: string; onChange: (v: string) => void; label: string }) {
  const [checks, setChecks] = useState<{ name: string; instance: string }[]>([]);
  useEffect(() => {
    if (!serverId) return;
    const controller = new AbortController();
    api
      .serverSensors(serverId, controller.signal)
      .then((list) => setChecks(list.sensors.map((s) => ({ name: s.name, instance: s.instance }))))
      .catch(() => setChecks([]));
    return () => controller.abort();
  }, [serverId]);
  const known = value === "" || checks.some((c) => checkKey(c.name, c.instance) === value);
  return (
    <select className={fieldClass} aria-label={label} value={value} onChange={(e) => onChange(e.target.value)} disabled={!serverId}>
      <option value="">{t("reports.service.wholeServer")}</option>
      {!known && <option value={value}>{value.replaceAll("\t", " ").trim()}</option>}
      {checks.map((c) => (
        <option key={checkKey(c.name, c.instance)} value={checkKey(c.name, c.instance)}>
          {c.instance ? `${c.name} (${c.instance})` : c.name}
        </option>
      ))}
    </select>
  );
}

function ServiceForm({
  service,
  customers,
  servers,
  defaultCustomer,
  onSaved,
  onCancel,
}: {
  service: BusinessService | null;
  customers: Customer[];
  servers: Server[];
  defaultCustomer: string;
  onSaved: () => void;
  onCancel: () => void;
}) {
  const multiCustomer = useMultiCustomer();
  const [draft, setDraft] = useState(() => draftOf(service, defaultCustomer));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setDraft((d) => ({ ...d, [key]: value }));
  const setMember = (i: number, member: MemberDraft) => set("members", draft.members.map((m, j) => (j === i ? member : m)));
  const choices = servers
    .filter((s) => !multiCustomer || !draft.customerId || s.customer_id === draft.customerId)
    .sort((a, b) => a.hostname.localeCompare(b.hostname));
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const request = serviceRequestOf(draft, multiCustomer);
    if (typeof request === "string") {
      setError(request);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      if (service) await api.updateBusinessService(service.id, request);
      else await api.createBusinessService(request);
      onSaved();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form className={styles.form} onSubmit={submit} aria-label={service ? t("reports.service.change", { name: service.name }) : t("reports.service.add")}>
      {multiCustomer && (
        <label className={admin.field}>
          {t("reports.service.owner")}
          <select className={fieldClass} required value={draft.customerId} onChange={(e) => setDraft({ ...draft, customerId: e.target.value, members: [{ serverId: "", check: "" }] })}>
            <option value="">{t("reports.service.choose")}</option>
            {customers.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className={admin.field}>
        {t("reports.service.name")}
        <input className={fieldClass} required maxLength={80} value={draft.name} placeholder={t("reports.service.namePlaceholder")} onChange={(e) => set("name", e.target.value)} />
      </label>
      <label className={admin.field}>
        {t("reports.service.description")}
        <input className={fieldClass} maxLength={500} value={draft.description} onChange={(e) => set("description", e.target.value)} />
      </label>
      <fieldset className={styles.fieldset}>
        <legend>{t("reports.service.members")}</legend>
        {draft.members.map((member, i) => (
          <div key={i} className={styles.memberRow}>
            <select
              className={fieldClass}
              aria-label={t("reports.service.memberServer", { count: i + 1 })}
              value={member.serverId}
              onChange={(e) => setMember(i, { serverId: e.target.value, check: "" })}
            >
              <option value="">{t("reports.service.choose")}</option>
              {choices.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.hostname}
                </option>
              ))}
            </select>
            <CheckPicker
              serverId={member.serverId}
              value={member.check}
              label={t("reports.service.memberCheck", { count: i + 1 })}
              onChange={(check) => setMember(i, { ...member, check })}
            />
            {draft.members.length > 1 && (
              <Button onClick={() => set("members", draft.members.filter((_, j) => j !== i))}>{t("reports.service.removeMember")}</Button>
            )}
          </div>
        ))}
        <div>
          <Button onClick={() => set("members", [...draft.members, { serverId: "", check: "" }])}>{t("reports.service.addMember")}</Button>
        </div>
      </fieldset>
      <fieldset className={styles.fieldset}>
        <legend>{t("reports.service.rule")}</legend>
        <div className={styles.choice}>
          <SegmentedControl
            label={t("reports.service.rule")}
            options={[
              { value: "all", label: t("reports.service.ruleAllLong") },
              { value: "atLeast", label: t("reports.service.ruleAtLeastLong") },
            ]}
            value={draft.atLeast ? "atLeast" : "all"}
            onChange={(v) => set("atLeast", v === "atLeast")}
          />
          <input
            className={fieldClass}
            type="number"
            min={1}
            max={Math.max(1, draft.members.length)}
            aria-label={t("reports.service.count")}
            value={draft.count}
            disabled={!draft.atLeast}
            onChange={(e) => set("count", Number(e.target.value))}
          />
        </div>
      </fieldset>
      <label className={admin.field}>
        {t("reports.service.target")}
        <input className={fieldClass} inputMode="decimal" value={draft.target} placeholder="99.9" onChange={(e) => set("target", e.target.value)} />
        <span className={admin.fieldNote}>{t("reports.service.targetNote")}</span>
      </label>
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("reports.saving") : service ? t("reports.save") : t("reports.service.create")}
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

const loadServices = (signal: AbortSignal) => api.businessServices(signal);

/** Business services on the Reports page: the groups with one availability, and a way to make them. */
export function BusinessServicesSection({ onChanged }: { onChanged?: () => void }) {
  const me = useMe();
  const scope = useScope();
  const multiCustomer = useMultiCustomer();
  const { data, error, refresh } = usePolledData(loadServices, { intervalMs: 5 * 60_000 });
  const [editing, setEditing] = useState<BusinessService | "new" | null>(null);
  const [removing, setRemoving] = useState<string | null>(null);
  const [places, setPlaces] = useState<{ customers: Customer[]; servers: Server[] } | null>(null);
  const loadPlaces = useCallback(async () => {
    const [customers, servers] = await Promise.all([api.customers(), api.servers()]);
    setPlaces({ customers: customers.customers.filter((c) => modifiesCustomer(me, c.id)), servers: servers.servers });
  }, [me]);
  const canAdd = me?.roles.some((g) => g.role !== "viewer" && g.site_id === null) ?? false;
  const services = (data?.services ?? []).filter((s) => !scope.customerId || s.customer_id === scope.customerId);
  const open = (target: BusinessService | "new") => {
    setEditing(target);
    if (!places) void loadPlaces().catch(() => setPlaces({ customers: [], servers: [] }));
  };
  const saved = () => {
    setEditing(null);
    refresh();
    onChanged?.();
  };
  const columns: Column<BusinessService>[] = [
    { key: "name", header: t("reports.column.service"), cell: (s) => s.name },
    ...(multiCustomer ? [{ key: "owner", header: t("reports.service.owner"), cell: (s: BusinessService) => s.customer_name }] : []),
    { key: "rule", header: t("reports.service.rule"), cell: (s) => describeRule(s) },
    { key: "members", header: t("reports.service.members"), cell: (s) => s.members.map((m) => (m.check ? `${m.server_name} ${m.check.name}` : m.server_name)).join(", ") },
    { key: "target", header: t("reports.column.target"), cell: (s) => (s.target === null ? t("reports.none") : formatTarget(s.target)) },
  ];
  if (!canAdd && data && services.length === 0) return null;
  return (
    <section aria-label={t("reports.service.manage")} className={styles.section}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("reports.service.manage")}</h2>
        {canAdd && editing === null && (
          <Button variant="k" onClick={() => open("new")}>
            {t("reports.service.add")}
          </Button>
        )}
      </div>
      <p className={admin.fieldNote}>{t("reports.service.intro")}</p>
      {editing !== null &&
        (places ? (
          <ServiceForm
            service={editing === "new" ? null : editing}
            customers={places.customers}
            servers={places.servers}
            defaultCustomer={scope.customerId ?? ""}
            onSaved={saved}
            onCancel={() => setEditing(null)}
          />
        ) : (
          <p className={admin.fieldNote}>{t("reports.loading")}</p>
        ))}
      {!data && error !== null && !isUnauthorized(error) ? (
        <ErrorState what={t("reports.service.load")} error={describeError(error)} onRetry={refresh} />
      ) : data && services.length === 0 ? (
        <EmptyState>{t("reports.service.empty")}</EmptyState>
      ) : (
        <DataTable
          label={t("reports.service.manage")}
          columns={columns}
          rows={services}
          rowKey={(s) => s.id}
          loading={!data}
          rowActions={(s) =>
            s.can_change && (
              <RowMenu
                label={t("reports.service.actions", { name: s.name })}
                items={[
                  { label: t("reports.change"), onSelect: () => open(s) },
                  { label: t("reports.remove"), onSelect: () => setRemoving(s.id), danger: true },
                ]}
              />
            )
          }
          rowDetail={(s) =>
            removing === s.id && (
              <InlineConfirm
                layout="row"
                trigger={t("reports.remove")}
                title={t("reports.service.removeTitle", { name: s.name })}
                consequence={t("reports.service.removeConsequence")}
                confirmLabel={t("reports.remove")}
                describeError={describeError}
                onClose={() => setRemoving(null)}
                onConfirm={async () => {
                  await api.removeBusinessService(s.id);
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
