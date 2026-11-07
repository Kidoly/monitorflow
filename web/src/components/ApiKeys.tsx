"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useEffect, useState } from "react";

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
import { administersCustomer, administersEverything, administersSite, useMe } from "@/lib/me";
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

/** What a key may do, in the words of the glossary. */
export const ACCESS_WORDS: Record<Role, string> = {
  viewer: "Read only",
  operator: "Read and change",
  approver: "Approve commands",
  administrator: "Administer",
};

export const AREA_WORDS: Record<ApiKeyArea, string> = {
  servers: "Servers",
  alerts: "Alerts and checks",
  updates: "Updates and commands",
  remediation: "AI remediation",
  dashboards: "Dashboards",
  administration: "Administration",
};

const AREAS = Object.keys(AREA_WORDS) as ApiKeyArea[];

const EXPIRIES: { value: ApiKeyExpiry; label: string }[] = [
  { value: "1y", label: "In a year" },
  { value: "90d", label: "In 90 days" },
  { value: "30d", label: "In 30 days" },
  { value: "never", label: "Never" },
];

/** Where a key reaches, in plain words; customers only once there are two. */
export function reachWords(key: ApiKey, manyCustomers: boolean): string {
  if (key.site_id) return manyCustomers ? `${key.customer_name} › ${key.site_name}` : `${key.site_name}`;
  if (key.customer_id) return `${key.customer_name}`;
  return "Everything";
}

/** "Read and change · Alerts and checks, Servers" */
export function accessWords(key: ApiKey): string {
  const areas = key.areas ? key.areas.map((a) => AREA_WORDS[a]).join(", ") : "every area";
  return `${ACCESS_WORDS[key.role]} · ${areas}`;
}

/**
 * `/admin/api-keys/` (ADR-0101): the keys an administrator could have made,
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
  const manyCustomers = customers.length > 1;

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
      title="API keys"
      meta={
        <>
          For scripts and other tools, like a PSA or Ansible · <Link href={API_DOCS_PATH}>API documentation</Link>
        </>
      }
    />
  );
  if (!data) {
    return (
      <div className={admin.page}>
        {header}
        {error ? <ErrorState what="the API keys" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  return (
    <div className={admin.page}>
      {header}
      {created && (
        <>
          <Banner tone="warn">
            Copy the key for “{created.name}” now: it is shown only once. Whoever has it can do what it allows until it
            expires or you turn it off.
          </Banner>
          <SecretField label="API key" value={created.secret} copyName="Copy the key" />
        </>
      )}
      {data.keys.length === 0 ? (
        <EmptyState title="No API keys yet">
          A key lets a script or another tool use MonitorFlow as you would, within what you choose.
        </EmptyState>
      ) : (
        <section className={admin.section} aria-label="API keys">
          <ul className={admin.linkList}>
            {data.keys.map((key) => (
              <li key={key.id}>
                <b>{key.name}</b> <span className={admin.mono}>mfk_{key.prefix}…</span>{" "}
                <span className={admin.dim}>
                  {accessWords(key)} · {reachWords(key, manyCustomers)}. Made by {key.created_by_username}.{" "}
                  {key.expires_at ? `Expires ${formatDay(new Date(key.expires_at), now)}.` : "Never expires."}{" "}
                  {key.last_used_at ? `Last used ${formatDay(new Date(key.last_used_at), now)}.` : "Not used yet."}
                </span>{" "}
                <InlineConfirm
                  layout="row"
                  trigger="Turn off"
                  triggerVariant="default"
                  triggerName={`Turn off ${key.name}`}
                  title={`Turn off ${key.name}?`}
                  consequence="Scripts and tools using it are refused from their next request."
                  confirmLabel="Turn off"
                  pendingLabel="Turning off…"
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
            New API key
          </Button>
        </div>
      )}
    </div>
  );
}

/** Reaches the user may give a key, as `everything`, `c:<id>` or `s:<id>`. */
export function reachOptions(
  me: Parameters<typeof administersEverything>[0],
  customers: Customer[],
  sites: Site[],
): { value: string; label: string }[] {
  const options: { value: string; label: string }[] = [];
  if (administersEverything(me)) options.push({ value: "everything", label: "Everything" });
  const many = customers.length > 1;
  if (many) {
    for (const c of customers) {
      if (administersCustomer(me, c.id)) options.push({ value: `c:${c.id}`, label: c.name });
    }
  }
  for (const s of sites) {
    if (administersSite(me, s.customer_id, s.id)) {
      options.push({
        value: `s:${s.id}`,
        label: many ? `${s.customer_name} › ${s.name}` : s.name,
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
  const reaches = reachOptions(me, customers, sites);
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
    if (!name.trim()) return setFailure("Say what the key is for, like “HaloPSA”.");
    if (limited && areas.length === 0) return setFailure("Choose at least one area.");
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
    <form className={`${admin.panel} ${admin.stack}`} onSubmit={(e) => void submit(e)} aria-label="New API key">
      <label className={admin.field}>
        What is it for?
        <input
          className={fieldClass}
          required
          maxLength={MAX_NAME}
          value={name}
          placeholder="HaloPSA"
          onChange={(e) => setName(e.target.value)}
        />
      </label>
      <label className={admin.field}>
        Access
        <select className={fieldClass} value={role} onChange={(e) => setRole(e.target.value as Role)}>
          {(Object.keys(ACCESS_WORDS) as Role[]).map((r) => (
            <option key={r} value={r}>
              {ACCESS_WORDS[r]}
            </option>
          ))}
        </select>
        <span className={admin.fieldNote}>Never more than you may do yourself, and less if you lose a role.</span>
      </label>
      {reaches.length > 1 && (
        <label className={admin.field}>
          Reaches
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
        <legend className={admin.legend}>Areas</legend>
        <label>
          <input type="checkbox" checked={limited} onChange={(e) => setLimited(e.target.checked)} /> Only some areas
        </label>
        {limited &&
          AREAS.map((area) => (
            <label key={area}>
              <input
                type="checkbox"
                checked={areas.includes(area)}
                onChange={(e) => setAreas(e.target.checked ? [...areas, area] : areas.filter((a) => a !== area))}
              />{" "}
              {AREA_WORDS[area]}
            </label>
          ))}
        <span className={admin.fieldNote}>Any key may list servers, sites and customers within its reach.</span>
      </fieldset>
      <label className={admin.field}>
        The key stops working
        <select className={fieldClass} value={expires} onChange={(e) => setExpires(e.target.value as ApiKeyExpiry)}>
          {EXPIRIES.map((x) => (
            <option key={x.value} value={x.value}>
              {x.label}
            </option>
          ))}
        </select>
      </label>
      {failure && <Banner tone="bad">{failure}</Banner>}
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? "Making…" : "Make the key"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
