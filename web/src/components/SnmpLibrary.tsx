"use client";

import { type FormEvent, useCallback, useEffect, useId, useState } from "react";

import { t } from "@/i18n";
import {
  type Customer,
  type SnmpLookup,
  type SnmpLookupEntry,
  type SnmpMib,
  type SnmpOfferedLookup,
  type SnmpState,
  api,
  describeError,
} from "@/lib/api";
import { isAdministrator, modifiesCustomer, multiCustomer, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { Banner, NoPermission, PageHeader } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { TableBox } from "@/components/ui/TableBox";
import { formatExact } from "@/components/ui/time";

import admin from "./AdminPages.module.css";

/** Most MIB files one upload takes, as the API does. */
export const MAX_FILES = 20;
/** Largest MIB file, as the API takes it. */
export const MAX_FILE_BYTES = 1 << 20;

const STATES: SnmpState[] = ["ok", "warning", "critical"];

const stateWord = (state: SnmpState) => t(`snmp.state.${state}` as "snmp.state.ok");
const stateTone = (state: SnmpState) => (state === "ok" ? "ok" : state === "warning" ? "warn" : "bad");

/** "1 = OK, 2 = Warning, 3 = Failed". */
export function entriesText(entries: SnmpLookupEntry[]): string {
  const shown = entries.slice(0, 6).map((e) => {
    const value = e.text !== undefined ? `“${e.text}”` : e.max !== undefined ? `${e.value}..${e.max}` : String(e.value);
    return `${value} = ${e.name}`;
  });
  return entries.length > 6 ? `${shown.join(", ")}, …` : shown.join(", ");
}

/** One entry as the form holds it. */
interface EntryForm {
  kind: "value" | "range" | "text";
  value: string;
  max: string;
  text: string;
  name: string;
  state: SnmpState;
}

const NEW_ENTRY: EntryForm = { kind: "value", value: "", max: "", text: "", name: "", state: "ok" };

function entryForm(e: SnmpLookupEntry): EntryForm {
  return {
    kind: e.text !== undefined ? "text" : e.max !== undefined ? "range" : "value",
    value: e.value !== undefined ? String(e.value) : "",
    max: e.max !== undefined ? String(e.max) : "",
    text: e.text ?? "",
    name: e.name,
    state: e.state,
  };
}

/** The entries of the form, or the number of the first one that does not read. */
export function entriesOf(forms: EntryForm[]): SnmpLookupEntry[] | number {
  const out: SnmpLookupEntry[] = [];
  for (const [i, f] of forms.entries()) {
    const integer = (s: string) => (/^-?\d+$/.test(s.trim()) ? Number(s.trim()) : null);
    if (f.name.trim() === "") return i + 1;
    if (f.kind === "text") {
      out.push({ text: f.text, name: f.name.trim(), state: f.state });
      continue;
    }
    const value = integer(f.value);
    if (value === null) return i + 1;
    if (f.kind === "range") {
      const max = integer(f.max);
      if (max === null || max < value) return i + 1;
      out.push({ value, max, name: f.name.trim(), state: f.state });
    } else {
      out.push({ value, name: f.name.trim(), state: f.state });
    }
  }
  return out;
}

function LookupForm({
  initial,
  customers,
  onDone,
  onCancel,
}: {
  initial: { id?: string; name: string; entries: SnmpLookupEntry[]; otherwise: SnmpState };
  customers: Customer[] | null;
  onDone: () => void;
  onCancel: () => void;
}) {
  const titleId = useId();
  const [name, setName] = useState(initial.name);
  const [entries, setEntries] = useState<EntryForm[]>(initial.entries.length ? initial.entries.map(entryForm) : [NEW_ENTRY]);
  const [otherwise, setOtherwise] = useState<SnmpState>(initial.otherwise);
  const [customer, setCustomer] = useState(customers?.[0]?.id ?? "");
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const setEntry = (i: number, patch: Partial<EntryForm>) => setEntries((all) => all.map((e, j) => (j === i ? { ...e, ...patch } : e)));

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (name.trim() === "") return setProblem(t("snmp.lookups.error.name"));
    const checked = entriesOf(entries);
    if (typeof checked === "number") return setProblem(t("snmp.lookups.error.entry", { n: checked }));
    setBusy(true);
    setProblem(null);
    const body = { name: name.trim(), entries: checked, otherwise, ...(customers && !initial.id && customer && { customer_id: customer }) };
    try {
      if (initial.id) await api.updateSnmpLookup(initial.id, body);
      else await api.createSnmpLookup(body);
      onDone();
    } catch (e) {
      setProblem(t("snmp.lookups.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <form className={admin.panel} onSubmit={(e) => void submit(e)} aria-labelledby={titleId} noValidate>
      <h2 id={titleId} className={admin.sectionTitle}>
        {initial.id ? t("snmp.lookups.editTitle", { name: initial.name }) : t("snmp.lookups.newTitle")}
      </h2>
      {customers && !initial.id && (
        <label className={admin.field}>
          {t("snmp.lookups.customer")}
          <select className={fieldClass} value={customer} onChange={(e) => setCustomer(e.target.value)}>
            {customers.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className={admin.field}>
        {t("snmp.lookups.name")}
        <input className={fieldClass} value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
      </label>
      {entries.map((entry, i) => (
        <fieldset key={i} className={admin.panel}>
          <legend className={admin.sectionTitle}>{t("snmp.lookups.entry", { n: i + 1 })}</legend>
          <label className={admin.field}>
            {t("snmp.lookups.kind")}
            <select className={fieldClass} value={entry.kind} onChange={(e) => setEntry(i, { kind: e.target.value as EntryForm["kind"] })}>
              <option value="value">{t("snmp.lookups.kind.value")}</option>
              <option value="range">{t("snmp.lookups.kind.range")}</option>
              <option value="text">{t("snmp.lookups.kind.text")}</option>
            </select>
          </label>
          {entry.kind === "text" ? (
            <label className={admin.field}>
              {t("snmp.lookups.text")}
              <input className={fieldClass} value={entry.text} onChange={(e) => setEntry(i, { text: e.target.value })} />
            </label>
          ) : (
            <label className={admin.field}>
              {entry.kind === "range" ? t("snmp.lookups.from") : t("snmp.lookups.value")}
              <input className={fieldClass} inputMode="numeric" value={entry.value} onChange={(e) => setEntry(i, { value: e.target.value })} />
            </label>
          )}
          {entry.kind === "range" && (
            <label className={admin.field}>
              {t("snmp.lookups.to")}
              <input className={fieldClass} inputMode="numeric" value={entry.max} onChange={(e) => setEntry(i, { max: e.target.value })} />
            </label>
          )}
          <label className={admin.field}>
            {t("snmp.lookups.entryName")}
            <input className={fieldClass} value={entry.name} maxLength={64} onChange={(e) => setEntry(i, { name: e.target.value })} />
          </label>
          <label className={admin.field}>
            {t("snmp.column.state")}
            <select className={fieldClass} value={entry.state} onChange={(e) => setEntry(i, { state: e.target.value as SnmpState })}>
              {STATES.map((s) => (
                <option key={s} value={s}>
                  {stateWord(s)}
                </option>
              ))}
            </select>
          </label>
          {entries.length > 1 && (
            <Button variant="ghost" size="small" onClick={() => setEntries((all) => all.filter((_, j) => j !== i))}>
              {t("snmp.lookups.removeEntry")}
            </Button>
          )}
        </fieldset>
      ))}
      <div className={admin.formFoot}>
        {entries.length < 100 && <Button onClick={() => setEntries((all) => [...all, NEW_ENTRY])}>{t("snmp.lookups.addEntry")}</Button>}
      </div>
      <label className={admin.field}>
        {t("snmp.lookups.otherwise")}
        <select className={fieldClass} value={otherwise} onChange={(e) => setOtherwise(e.target.value as SnmpState)}>
          {STATES.map((s) => (
            <option key={s} value={s}>
              {stateWord(s)}
            </option>
          ))}
        </select>
      </label>
      <div className={admin.formFoot}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("snmp.saving") : t("snmp.lookups.save")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("snmp.cancel")}
        </Button>
      </div>
      {problem && (
        <p className={admin.formError} role="alert">
          {problem}
        </p>
      )}
    </form>
  );
}

function Upload({ customers, onUploaded }: { customers: Customer[] | null; onUploaded: (offered: SnmpOfferedLookup[], added: SnmpMib[]) => void }) {
  const inputId = useId();
  const [customer, setCustomer] = useState(customers?.[0]?.id ?? "");
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function upload(list: FileList | null) {
    const files = [...(list ?? [])];
    if (files.length === 0) return;
    if (files.length > MAX_FILES) return setProblem(t("snmp.mibs.tooMany", { count: MAX_FILES }));
    const big = files.find((f) => f.size > MAX_FILE_BYTES);
    if (big) return setProblem(t("snmp.mibs.tooBig", { name: big.name }));
    setBusy(true);
    setProblem(null);
    try {
      const texts = await Promise.all(files.map(async (f) => ({ name: f.name, text: await f.text() })));
      const result = await api.uploadSnmpMibs({ files: texts, ...(customers && customer && { customer_id: customer }) });
      onUploaded(result.offered_lookups, result.mibs);
    } catch (e) {
      setProblem(t("snmp.mibs.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className={admin.panel}>
      {customers && (
        <label className={admin.field}>
          {t("snmp.lookups.customer")}
          <select className={fieldClass} value={customer} onChange={(e) => setCustomer(e.target.value)}>
            {customers.map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className={admin.field} htmlFor={inputId}>
        {busy ? t("snmp.mibs.uploading") : t("snmp.mibs.upload")}
        <input
          id={inputId}
          type="file"
          multiple
          accept=".mib,.my,.txt,.smi,text/plain"
          disabled={busy}
          onChange={(e) => {
            void upload(e.target.files);
            e.target.value = "";
          }}
        />
        <span className={admin.sectionNote}>{t("snmp.mibs.uploadHint", { count: MAX_FILES })}</span>
      </label>
      {problem && (
        <p className={admin.formError} role="alert">
          {problem}
        </p>
      )}
    </div>
  );
}

/**
 * Settings › SNMP lookups and MIBs (ADR-0157): the built-in lookups and
 * the customer's own, and the MIB files that name its devices' objects.
 */
export function SnmpLibraryPage() {
  const me = useMe();
  const lookupsFetcher = useCallback((signal: AbortSignal) => api.snmpLookups(signal), []);
  const mibsFetcher = useCallback((signal: AbortSignal) => api.snmpMibs(signal), []);
  const lookups = usePolledData(lookupsFetcher);
  const mibs = usePolledData(mibsFetcher);
  const several = multiCustomer(me);
  const [customers, setCustomers] = useState<Customer[] | null>(null);
  // null: closed; else the lookup to make or change.
  const [editing, setEditing] = useState<{ id?: string; name: string; entries: SnmpLookupEntry[]; otherwise: SnmpState } | null>(null);
  const [offered, setOffered] = useState<SnmpOfferedLookup[]>([]);
  const [added, setAdded] = useState<SnmpMib[] | null>(null);
  useEffect(() => {
    if (!several) return;
    const controller = new AbortController();
    api.customers(controller.signal).then(
      (r) => setCustomers(r.customers.filter((c) => modifiesCustomer(me, c.id))),
      () => setCustomers([]),
    );
    return () => controller.abort();
  }, [several, me]);
  if (!me || !isAdministrator(me)) {
    return <NoPermission title={t("snmp.lookups.needAdmin")}>{t("snmp.lookups.needAdminBody")}</NoPermission>;
  }
  const refresh = () => {
    lookups.refresh();
    mibs.refresh();
  };

  const lookupColumns: Column<SnmpLookup>[] = [
    {
      key: "name",
      header: t("snmp.lookups.name"),
      cell: (l) => <PrimaryCell title={l.name} sub={l.builtin ? t("snmp.lookups.builtinWord") : undefined} />,
    },
    { key: "entries", header: t("snmp.lookups.entries"), cell: (l) => entriesText(l.entries) },
    {
      key: "otherwise",
      header: t("snmp.lookups.otherwise"),
      cell: (l) => <Label tone={stateTone(l.otherwise)}>{stateWord(l.otherwise)}</Label>,
    },
    { key: "used", header: t("snmp.lookups.usedBy"), cell: (l) => t("snmp.lookups.checks", { count: l.used_by }) },
  ];
  const mibColumns: Column<SnmpMib>[] = [
    { key: "module", header: t("snmp.mibs.module"), cell: (m) => <PrimaryCell title={m.module} sub={m.file_name} /> },
    { key: "objects", header: t("snmp.mibs.objects"), cell: (m) => t("snmp.mibs.objectCount", { count: m.objects }) },
    { key: "uploaded", header: t("snmp.mibs.uploaded"), cell: (m) => formatExact(new Date(m.uploaded_at)).slice(0, 16) },
  ];

  return (
    <Page>
      <PageHeader title={t("snmp.library.title")} />
      <p className={admin.sectionNote}>{t("snmp.library.note")}</p>
      {Boolean(lookups.error || mibs.error) && !(lookups.data && mibs.data) && (
        <Banner tone="warn">{t("snmp.library.loadFailed", { error: describeError(lookups.error ?? mibs.error) })}</Banner>
      )}
      <TableBox title={t("snmp.mibs.title")}>
        <Upload
          customers={several ? customers : null}
          onUploaded={(found, modules) => {
            setOffered(found);
            setAdded(modules);
            refresh();
          }}
        />
        {added && <p className={admin.sectionNote}>{t("snmp.mibs.added", { modules: added.map((m) => m.module).join(", ") })}</p>}
        {offered.length > 0 && (
          <div className={admin.panel} role="group" aria-label={t("snmp.mibs.offered")}>
            <h3 className={admin.sectionTitle}>{t("snmp.mibs.offered")}</h3>
            <ul>
              {offered.slice(0, 20).map((o) => (
                <li key={`${o.module}/${o.name}`}>
                  {o.name} ({entriesText(o.entries)}){" "}
                  <Button
                    size="small"
                    onClick={() => setEditing({ name: o.name, entries: o.entries, otherwise: "warning" })}
                    aria-label={t("snmp.mibs.makeLookupOf", { name: o.name })}
                  >
                    {t("snmp.mibs.makeLookup")}
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}
        {mibs.data && mibs.data.mibs.length > 0 && (
          <DataTable
            bare
            label={t("snmp.mibs.title")}
            columns={mibColumns}
            rows={mibs.data.mibs}
            rowKey={(m) => m.id}
            rowActions={(m) =>
              m.can_change && (
                <InlineConfirm
                  trigger={t("snmp.remove")}
                  triggerName={`${t("snmp.remove")} ${m.module}`}
                  triggerVariant="ghost"
                  layout="row"
                  title={t("snmp.mibs.removeConfirm", { module: m.module })}
                  consequence=""
                  confirmLabel={`${t("snmp.remove")} ${m.module}`}
                  onConfirm={async () => {
                    await api.deleteSnmpMib(m.id);
                    refresh();
                  }}
                />
              )
            }
          />
        )}
      </TableBox>
      <TableBox
        title={t("snmp.lookups.title")}
        meta={
          !editing && (
            <Button size="small" onClick={() => setEditing({ name: "", entries: [], otherwise: "warning" })}>
              {t("snmp.lookups.new")}
            </Button>
          )
        }
      >
        {editing && (
          <LookupForm
            initial={editing}
            customers={several ? customers : null}
            onDone={() => {
              setEditing(null);
              refresh();
            }}
            onCancel={() => setEditing(null)}
          />
        )}
        {lookups.data && (
          <DataTable
            bare
            label={t("snmp.lookups.title")}
            columns={lookupColumns}
            rows={lookups.data.lookups}
            rowKey={(l) => l.id}
            rowActions={(l) =>
              l.can_change && (
                <>
                  <Button variant="ghost" size="small" onClick={() => setEditing(l)} aria-label={`${t("snmp.edit")} ${l.name}`}>
                    {t("snmp.edit")}
                  </Button>
                  <InlineConfirm
                    trigger={t("snmp.remove")}
                    triggerName={`${t("snmp.remove")} ${l.name}`}
                    triggerVariant="ghost"
                    layout="row"
                    title={t("snmp.lookups.removeConfirm", { name: l.name })}
                    consequence=""
                    confirmLabel={`${t("snmp.remove")} ${l.name}`}
                    describeError={(e) => t("snmp.lookups.removeFailed", { error: describeError(e) })}
                    onConfirm={async () => {
                      await api.deleteSnmpLookup(l.id);
                      refresh();
                    }}
                  />
                </>
              )
            }
          />
        )}
      </TableBox>
    </Page>
  );
}
