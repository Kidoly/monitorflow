"use client";

import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  type Customer,
  type Me,
  type Mirror,
  type SoftwareEntry,
  type SoftwareManager,
  type SoftwareRequest,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import { validLinuxPackage, validWingetId } from "@/lib/campaigns";
import { administersCustomer, administersEverything, multiCustomer, useMe } from "@/lib/me";
import { CAMPAIGNS_PATH } from "@/lib/routes";

import own from "./Campaigns.module.css";
import { Breadcrumb } from "./ui/Breadcrumb";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Missing } from "./ui/Missing";
import { Banner, ErrorState, LoadingBlock, PageHeader } from "./ui/PageState";

/** Limits of `POST /api/software` (ADR-0188). */
const MAX_NAME = 128;
const MAX_DESCRIPTION = 500;
const SOURCE = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

/** The managers of the catalogue, in the order the select offers them. */
const MANAGERS: SoftwareManager[] = ["winget", "apt", "dnf"];

/** The value of the customer select for an entry shared by every customer. */
const SHARED = "";

interface Draft {
  /** The entry edited, or null for a new one. */
  id: string | null;
  customerId: string;
  name: string;
  manager: SoftwareManager;
  packageId: string;
  source: string;
  /** apt and dnf: a mirror name, or "" for the servers' own sources. */
  mirror: string;
  description: string;
}

const emptyDraft = (customerId: string): Draft => ({
  id: null,
  customerId,
  name: "",
  manager: "winget",
  packageId: "",
  source: "winget",
  mirror: "",
  description: "",
});

/** The request for `draft`, trimmed. */
export function softwareRequest(draft: Draft): SoftwareRequest {
  const common = {
    customer_id: draft.customerId || null,
    name: draft.name.trim(),
    manager: draft.manager,
    package_id: draft.packageId.trim(),
    description: draft.description.trim(),
  };
  // winget installs from a source; apt and dnf from the servers' sources or a mirror.
  return draft.manager === "winget"
    ? { ...common, source: draft.source.trim() }
    : { ...common, mirror: draft.mirror || null };
}

/** The errors the API would answer for `request`; empty when it looks valid. */
export function checkSoftware(request: SoftwareRequest): string[] {
  const errors: string[] = [];
  const name = [...request.name].length;
  if (name < 1 || name > MAX_NAME) errors.push(t("campaigns.software.check.name"));
  if ((request.manager ?? "winget") === "winget") {
    if (!validWingetId(request.package_id)) errors.push(t("campaigns.software.check.package"));
    const source = request.source ?? "";
    if (!SOURCE.test(source) || source.toLowerCase() === "msstore") errors.push(t("campaigns.software.check.source"));
  } else if (!validLinuxPackage(request.package_id)) {
    errors.push(t("campaigns.software.check.linuxPackage"));
  }
  if ([...(request.description ?? "")].length > MAX_DESCRIPTION) errors.push(t("campaigns.software.check.description"));
  return errors;
}

/** Whether `me` may change `entry`: global administrators the shared ones, customer administrators their own. */
const canChange = (me: Me | null, entry: Pick<SoftwareEntry, "customer_id">) =>
  entry.customer_id === null ? administersEverything(me) : administersCustomer(me, entry.customer_id);

/** `/campaigns/software/`: the software catalogue software campaigns install from (ADR-0188). */
export function SoftwareCatalog() {
  const me = useMe();
  const [entries, setEntries] = useState<SoftwareEntry[] | null>(null);
  const [customers, setCustomers] = useState<Customer[]>([]);
  const [mirrors, setMirrors] = useState<Mirror[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [nonce, setNonce] = useState(0);
  const reload = useCallback(() => setNonce((n) => n + 1), []);

  useEffect(() => {
    const controller = new AbortController();
    const { signal } = controller;
    Promise.all([api.softwareCatalog(undefined, signal), api.customers(signal)]).then(
      ([catalog, list]) => {
        setEntries(catalog.software);
        setCustomers(list.customers);
        setLoadError(null);
      },
      (e: unknown) => {
        if (!isAbortError(e)) setLoadError(describeError(e));
      },
    );
    // Mirrors are optional here: who cannot list them keeps the servers' own sources.
    api.mirrors(signal).then(
      (list) => setMirrors(list.mirrors.filter((m) => !m.deleting_at)),
      () => setMirrors([]),
    );
    return () => controller.abort();
  }, [nonce]);

  const header = (
    <>
      <Breadcrumb parents={[{ label: t("campaigns.label"), href: CAMPAIGNS_PATH }]} />
      <PageHeader detail title={t("campaigns.software.title")} meta={t("campaigns.software.intro")} />
    </>
  );
  if (!entries) {
    return (
      <div className={own.page}>
        {header}
        {loadError ? (
          <ErrorState what={t("campaigns.software.what")} error={loadError} onRetry={reload} />
        ) : (
          <LoadingBlock label={t("campaigns.software.loading")} />
        )}
      </div>
    );
  }
  return (
    <div className={own.page}>
      {header}
      <Catalog me={me} entries={entries} customers={customers} mirrors={mirrors} onChange={reload} />
    </div>
  );
}

function Catalog({
  me,
  entries,
  customers,
  mirrors,
  onChange,
}: {
  me: Me | null;
  entries: SoftwareEntry[];
  customers: Customer[];
  mirrors: Mirror[];
  onChange: () => void;
}) {
  const global = administersEverything(me);
  const administered = customers.filter((c) => administersCustomer(me, c.id));
  const defaultCustomer = global ? SHARED : (administered[0]?.id ?? SHARED);
  const [draft, setDraft] = useState<Draft>(() => emptyDraft(defaultCustomer));
  const [submitted, setSubmitted] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const showCustomers = multiCustomer(me);
  const customerName = (id: string | null) =>
    id === null ? t("campaigns.software.shared") : (customers.find((c) => c.id === id)?.name ?? id);

  const set = (patch: Partial<Draft>) => setDraft((d) => ({ ...d, ...patch }));
  const request = softwareRequest(draft);
  const errors = checkSoftware(request);
  const editing = draft.id !== null;
  const canAdd = global || administered.length > 0;

  function reset() {
    setDraft(emptyDraft(defaultCustomer));
    setSubmitted(false);
    setError(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    setSubmitted(true);
    setError(null);
    if (errors.length > 0) return;
    setBusy(true);
    try {
      if (draft.id) await api.updateSoftware(draft.id, request);
      else await api.createSoftware(request);
      reset();
      onChange();
    } catch (e) {
      setError(t(editing ? "campaigns.software.saveFailed" : "campaigns.software.addFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  function edit(entry: SoftwareEntry) {
    setDraft({
      id: entry.id,
      customerId: entry.customer_id ?? SHARED,
      name: entry.name,
      manager: entry.manager,
      packageId: entry.package_id,
      source: entry.manager === "winget" ? entry.source : "winget",
      mirror: entry.mirror ?? "",
      description: entry.description,
    });
    setSubmitted(false);
    setError(null);
  }

  const columns: Column<SoftwareEntry>[] = [
    { key: "name", header: t("campaigns.software.column.name"), cell: (e) => e.name, sortValue: (e) => e.name },
    {
      key: "package",
      header: t("campaigns.software.column.package"),
      cell: (e) => <code>{e.package_id}</code>,
      sortValue: (e) => e.package_id,
    },
    { key: "manager", header: t("campaigns.software.column.manager"), cell: (e) => e.manager, sortValue: (e) => e.manager },
    {
      key: "from",
      header: t("campaigns.software.column.from"),
      cell: (e) =>
        e.manager === "winget" ? e.source : e.mirror ? <code>{e.mirror}</code> : <span className={own.dim}>{t("campaigns.software.ownSources")}</span>,
    },
    ...(showCustomers
      ? [
          {
            key: "customer",
            header: t("campaigns.software.column.customer"),
            cell: (e: SoftwareEntry) => customerName(e.customer_id),
            sortValue: (e: SoftwareEntry) => customerName(e.customer_id),
          },
        ]
      : []),
    {
      key: "description",
      header: t("campaigns.software.column.description"),
      className: own.wide,
      cell: (e) => e.description || <Missing word="none" className={own.dim} />,
    },
  ];

  // Shared entries are offered only to global administrators; a customer stays as it is when editing.
  const customerOptions = [
    ...(global ? [{ value: SHARED, label: t("campaigns.software.sharedOption") }] : []),
    ...administered.map((c) => ({ value: c.id, label: c.name })),
  ];

  return (
    <>
      {entries.length === 0 ? (
        <p className={own.note}>{t("campaigns.software.empty")}</p>
      ) : (
        <DataTable
          label={t("campaigns.software.table")}
          columns={columns}
          rows={entries}
          rowKey={(e) => e.id}
          rowActions={(entry) =>
            canChange(me, entry) && (
              <div className={own.actions}>
                <Button size="small" aria-label={t("campaigns.software.editName", { name: entry.name })} onClick={() => edit(entry)}>
                  {t("campaigns.software.edit")}
                </Button>
                <InlineConfirm
                  layout="row"
                  trigger={t("campaigns.software.delete.trigger")}
                  triggerName={t("campaigns.software.delete.name", { name: entry.name })}
                  title={t("campaigns.software.delete.title", { name: entry.name })}
                  consequence={t("campaigns.software.delete.consequence")}
                  confirmLabel={t("campaigns.software.delete.confirm", { name: entry.name })}
                  onConfirm={async () => {
                    await api.deleteSoftware(entry.id);
                    if (draft.id === entry.id) reset();
                    onChange();
                  }}
                  describeError={(e) => t("campaigns.software.delete.failed", { error: describeError(e) })}
                />
              </div>
            )
          }
        />
      )}

      {canAdd && (
        <form
          className={own.fieldset}
          onSubmit={submit}
          aria-label={editing ? t("campaigns.software.editTitle", { name: draft.name }) : t("campaigns.software.addTitle")}
          noValidate
        >
          <div className={own.fields}>
            <strong>{editing ? t("campaigns.software.editTitle", { name: draft.name }) : t("campaigns.software.addTitle")}</strong>
            <div className={own.row}>
              <label className={own.fld}>
                {t("campaigns.software.column.name")}
                <input
                  className={fieldClass}
                  value={draft.name}
                  onChange={(e) => set({ name: e.target.value })}
                  maxLength={MAX_NAME}
                  required
                />
              </label>
              <label className={own.fld}>
                {t("campaigns.software.column.manager")}
                <select
                  className={fieldClass}
                  value={draft.manager}
                  onChange={(e) => set({ manager: e.target.value as SoftwareManager, mirror: "" })}
                >
                  {MANAGERS.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
              </label>
              <label className={own.fld}>
                {t("campaigns.software.column.package")}
                <input
                  className={fieldClass}
                  value={draft.packageId}
                  onChange={(e) => set({ packageId: e.target.value })}
                  placeholder={t(
                    draft.manager === "winget" ? "campaigns.software.packagePlaceholder" : "campaigns.software.linuxPlaceholder",
                  )}
                  spellCheck={false}
                  autoCapitalize="none"
                  required
                />
              </label>
              {draft.manager === "winget" ? (
                <label className={own.fld}>
                  {t("campaigns.software.column.source")}
                  <input
                    className={fieldClass}
                    value={draft.source}
                    onChange={(e) => set({ source: e.target.value })}
                    spellCheck={false}
                    autoCapitalize="none"
                  />
                </label>
              ) : (
                <label className={own.fld}>
                  {t("campaigns.software.column.mirror")}
                  <select className={fieldClass} value={draft.mirror} onChange={(e) => set({ mirror: e.target.value })}>
                    <option value="">{t("campaigns.software.noMirror")}</option>
                    {draft.mirror && !mirrors.some((m) => m.name === draft.mirror) && (
                      <option value={draft.mirror}>{draft.mirror}</option>
                    )}
                    {mirrors
                      .filter((m) => m.family === draft.manager)
                      .map((m) => (
                        <option key={m.id} value={m.name}>
                          {m.name}
                        </option>
                      ))}
                  </select>
                </label>
              )}
              {(showCustomers || global) && (
                <label className={own.fld}>
                  {t("campaigns.software.column.customer")}
                  <select
                    className={fieldClass}
                    value={draft.customerId}
                    disabled={editing}
                    onChange={(e) => set({ customerId: e.target.value })}
                  >
                    {editing && !customerOptions.some((o) => o.value === draft.customerId) && (
                      <option value={draft.customerId}>{customerName(draft.customerId || null)}</option>
                    )}
                    {customerOptions.map((o) => (
                      <option key={o.value} value={o.value}>
                        {o.label}
                      </option>
                    ))}
                  </select>
                </label>
              )}
            </div>
            <label className={own.fld}>
              {t("campaigns.software.column.description")}
              <input
                className={fieldClass}
                value={draft.description}
                onChange={(e) => set({ description: e.target.value })}
                maxLength={MAX_DESCRIPTION}
              />
            </label>
            {submitted && errors.length > 0 && (
              <Banner tone="bad">
                <ul className={own.errors}>
                  {errors.map((e) => (
                    <li key={e}>{e}</li>
                  ))}
                </ul>
              </Banner>
            )}
            {error && <Banner tone="bad">{error}</Banner>}
            <div className={own.row}>
              <Button variant="k" type="submit" disabled={busy}>
                {editing ? t("campaigns.software.save") : t("campaigns.software.add")}
              </Button>
              {editing && <Button onClick={reset}>{t("campaigns.software.cancel")}</Button>}
            </div>
          </div>
        </form>
      )}
    </>
  );
}
