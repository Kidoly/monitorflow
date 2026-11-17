"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  type ApiKey,
  type ApiKeyArea,
  type ApiKeyExpiry,
  type Customer,
  type Role,
  type Site,
  api,
  describeError,
} from "@/lib/api";
import { administersCustomer, administersEverything, administersSite, useMe, useMultiCustomer } from "@/lib/me";
import { API_DOCS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import admin from "./AdminPages.module.css";
import { Button, fieldClass } from "./ui/Button";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, LoadingBlock, PageHeader } from "./ui/PageState";
import { SecretField } from "./ui/SecretField";
import { formatDay } from "./ui/time";

const MAX_NAME = 80;

const ROLES: Role[] = ["viewer", "operator", "approver", "administrator"];

/** What a key may do, in the words of the glossary. */
export const accessWord = (role: Role) => t(`apiKeys.access.${role}`);

export const areaWord = (area: ApiKeyArea) => t(`apiKeys.area.${area}`);

const AREAS: ApiKeyArea[] = ["servers", "alerts", "updates", "remediation", "dashboards", "administration"];

const EXPIRIES: ApiKeyExpiry[] = ["1y", "90d", "30d", "never"];

/** Where a key reaches, in plain words; customers only for `multiCustomer` (ADR-0104). */
export function reachWords(key: ApiKey, multiCustomer: boolean): string {
  if (key.site_id) return multiCustomer ? `${key.customer_name} › ${key.site_name}` : `${key.site_name}`;
  if (key.customer_id) return `${key.customer_name}`;
  return t("apiKeys.everything");
}

/** "Read and change · Alerts and checks, Servers" */
export function accessWords(key: ApiKey): string {
  const areas = key.areas ? key.areas.map(areaWord).join(", ") : t("apiKeys.everyArea");
  return `${accessWord(key.role)} · ${areas}`;
}

/**
 * `/settings/api-keys/` (ADR-0101): the keys an administrator could have made,
 * "Turn off", and a form for a new one whose secret is shown once.
 */
export function ApiKeysPage() {
  return <AdminLayout>{() => <ApiKeys />}</AdminLayout>;
}

function ApiKeys() {
  const fetcher = useCallback((signal: AbortSignal) => api.apiKeys(signal), []);
  const { data, error, refresh } = usePolledData(fetcher, {
    intervalMs: 60_000,
  });
  const [customers, setCustomers] = useState<Customer[]>([]);
  const [sites, setSites] = useState<Site[]>([]);
  const [creating, setCreating] = useState(false);
  const [created, setCreated] = useState<{
    name: string;
    secret: string;
  } | null>(null);
  const now = new Date();
  const multiCustomer = useMultiCustomer();

  useEffect(() => {
    const controller = new AbortController();
    api.customers(controller.signal).then(
      (list) => setCustomers(list.customers),
      () => undefined,
    );
    api.sites(controller.signal).then(
      (list) => setSites(list.sites),
      () => undefined,
    );
    return () => controller.abort();
  }, []);

  const header = (
    <PageHeader
      title={t("apiKeys.title")}
      meta={
        <>
          {t("apiKeys.meta")} · <Link href={API_DOCS_PATH}>{t("apiKeys.docsLink")}</Link>
        </>
      }
    />
  );
  if (!data) {
    return (
      <div className={admin.page}>
        {header}
        {error ? <ErrorState what={t("apiKeys.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  return (
    <div className={admin.page}>
      {header}
      {created && (
        <>
          <Banner tone="warn">
            {t("apiKeys.copyNow", { name: created.name })}
          </Banner>
          <SecretField label={t("apiKeys.keyLabel")} value={created.secret} copyName={t("apiKeys.copyKey")} />
        </>
      )}
      {data.keys.length === 0 ? (
        <EmptyState title={t("apiKeys.emptyTitle")} />
      ) : (
        <section className={admin.section} aria-label={t("apiKeys.title")}>
          <ul className={admin.linkList}>
            {data.keys.map((key) => (
              <li key={key.id}>
                <b>{key.name}</b> <span className={admin.mono}>mfk_{key.prefix}…</span>{" "}
                <span className={admin.dim}>
                  {accessWords(key)} · {reachWords(key, multiCustomer)}.{" "}
                  {t("apiKeys.madeBy", { name: key.created_by_username })}{" "}
                  {key.expires_at
                    ? t("apiKeys.expires", { day: formatDay(new Date(key.expires_at), now) })
                    : t("apiKeys.neverExpires")}{" "}
                  {key.last_used_at
                    ? t("apiKeys.lastUsed", { day: formatDay(new Date(key.last_used_at), now) })
                    : t("apiKeys.notUsed")}
                </span>{" "}
                <InlineConfirm
                  layout="row"
                  trigger={t("apiKeys.turnOff")}
                  triggerVariant="default"
                  triggerName={t("apiKeys.turnOffName", { name: key.name })}
                  title={t("apiKeys.turnOffTitle", { name: key.name })}
                  consequence={t("apiKeys.turnOffConsequence")}
                  confirmLabel={t("apiKeys.turnOff")}
                  pendingLabel={t("apiKeys.turningOff")}
                  describeError={describeError}
                  onConfirm={async () => {
                    await api.revokeApiKey(key.id);
                    refresh();
                  }}
                />
              </li>
            ))}
          </ul>
        </section>
      )}
      {creating ? (
        <NewKey
          customers={customers}
          sites={sites}
          onCancel={() => setCreating(false)}
          onCreated={(key) => {
            setCreated(key);
            setCreating(false);
            refresh();
          }}
        />
      ) : (
        <div>
          <Button variant="k" onClick={() => setCreating(true)}>
            {t("apiKeys.newKey")}
          </Button>
        </div>
      )}
    </div>
  );
}

/**
 * Reaches the user may give a key, as `everything`, `c:<id>` or `s:<id>`;
 * customers only for `multiCustomer` (ADR-0104).
 */
export function reachOptions(
  me: Parameters<typeof administersEverything>[0],
  multiCustomer: boolean,
  customers: Customer[],
  sites: Site[],
): { value: string; label: string }[] {
  const options: { value: string; label: string }[] = [];
  if (administersEverything(me)) options.push({ value: "everything", label: t("apiKeys.everything") });
  if (multiCustomer) {
    for (const c of customers) {
      if (administersCustomer(me, c.id)) options.push({ value: `c:${c.id}`, label: c.name });
    }
  }
  for (const s of sites) {
    if (administersSite(me, s.customer_id, s.id)) {
      options.push({
        value: `s:${s.id}`,
        label: multiCustomer ? `${s.customer_name} › ${s.name}` : s.name,
      });
    }
  }
  return options;
}

function NewKey({
  customers,
  sites,
  onCancel,
  onCreated,
}: {
  customers: Customer[];
  sites: Site[];
  onCancel: () => void;
  onCreated: (key: { name: string; secret: string }) => void;
}) {
  const me = useMe();
  const multiCustomer = useMultiCustomer();
  const reaches = reachOptions(me, multiCustomer, customers, sites);
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("viewer");
  const [reach, setReach] = useState(reaches[0]?.value ?? "everything");
  const [limited, setLimited] = useState(false);
  const [areas, setAreas] = useState<ApiKeyArea[]>(["servers", "alerts"]);
  const [expires, setExpires] = useState<ApiKeyExpiry>("1y");
  const [busy, setBusy] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (!name.trim()) return setFailure(t("apiKeys.nameMissing"));
    if (limited && areas.length === 0) return setFailure(t("apiKeys.areaMissing"));
    setBusy(true);
    setFailure(null);
    try {
      const [kind, id] = reach.split(":");
      const created = await api.createApiKey({
        name: name.trim(),
        role,
        expires,
        ...(kind === "c" ? { customer_id: id } : {}),
        ...(kind === "s" ? { site_id: id } : {}),
        ...(limited ? { areas } : {}),
      });
      onCreated({ name: created.key.name, secret: created.secret });
    } catch (e) {
      setFailure(describeError(e));
      setBusy(false);
    }
  }

  return (
    <form className={`${admin.panel} ${admin.stack}`} onSubmit={(e) => void submit(e)} aria-label={t("apiKeys.newKey")}>
      <label className={admin.field}>
        {t("apiKeys.purpose")}
        <input
          className={fieldClass}
          required
          maxLength={MAX_NAME}
          value={name}
          placeholder={t("apiKeys.namePlaceholder")}
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <label className={admin.field}>
        {t("apiKeys.access")}
        <select className={fieldClass} value={role} onChange={(e) => setRole(e.target.value as Role)}>
          {ROLES.map((r) => (
            <option key={r} value={r}>
              {accessWord(r)}
            </option>
          ))}
        </select>
        <span className={admin.fieldNote}>{t("apiKeys.accessNote")}</span>
      </label>
      {reaches.length > 1 && (
        <label className={admin.field}>
          {t("apiKeys.reaches")}
          <select className={fieldClass} value={reach} onChange={(e) => setReach(e.target.value)}>
            {reaches.map((r) => (
              <option key={r.value} value={r.value}>
                {r.label}
              </option>
            ))}
          </select>
        </label>
      )}
      <fieldset className={admin.fieldset}>
        <legend className={admin.legend}>{t("apiKeys.areas")}</legend>
        <label>
          <input type="checkbox" checked={limited} onChange={(e) => setLimited(e.target.checked)} /> {t("apiKeys.someAreas")}
        </label>
        {limited &&
          AREAS.map((area) => (
            <label key={area}>
              <input
                type="checkbox"
                checked={areas.includes(area)}
                onChange={(e) => setAreas(e.target.checked ? [...areas, area] : areas.filter((a) => a !== area))}
              />{" "}
              {areaWord(area)}
            </label>
          ))}
        <span className={admin.fieldNote}>
          {multiCustomer
            ? t("apiKeys.listNoteCustomers")
            : t("apiKeys.listNote")}
        </span>
      </fieldset>
      <label className={admin.field}>
        {t("apiKeys.expiresField")}
        <select className={fieldClass} value={expires} onChange={(e) => setExpires(e.target.value as ApiKeyExpiry)}>
          {EXPIRIES.map((x) => (
            <option key={x} value={x}>
              {t(`apiKeys.expiry.${x}`)}
            </option>
          ))}
        </select>
      </label>
      {failure && <Banner tone="bad">{failure}</Banner>}
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? t("apiKeys.making") : t("apiKeys.make")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("apiKeys.cancel")}
        </Button>
      </div>
    </form>
  );
}
