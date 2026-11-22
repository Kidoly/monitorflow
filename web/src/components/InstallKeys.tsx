"use client";

import { type FormEvent, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Missing } from "@/components/ui/Missing";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { formatAgo } from "@/components/ui/time";
import { Switch } from "@/components/ui/Switch";
import { t } from "@/i18n";
import { type CreatedInstallKey, type InstallKey, type PendingHost, type Site, api, describeError } from "@/lib/api";
import { formatWhen } from "@/lib/clock";
import { placeLabel } from "@/lib/customers";
import { useMultiCustomer } from "@/lib/me";

import styles from "./AdminPages.module.css";

const DAY = 86_400;

/** Validity choices; the API takes one hour to one year, 30 days by default. */
const VALIDITY = [
  [DAY, "install.validity.1d"],
  [7 * DAY, "install.validity.7d"],
  [30 * DAY, "install.validity.30d"],
  [90 * DAY, "install.validity.90d"],
  [365 * DAY, "install.validity.1y"],
] as const;

/** "3 of 10 hosts", or "3 hosts" without a limit. */
export function hostsWords(key: Pick<InstallKey, "host_count" | "max_hosts">): string {
  return key.max_hosts === null
    ? t("install.hosts", { count: key.host_count })
    : t("install.hostsOf", { done: key.host_count, count: key.max_hosts });
}

/** The form that creates a site's install command (ADR-0073); every field has a default. */
export function CreateInstallKey({
  sites,
  onCreated,
}: {
  sites: Site[];
  onCreated: (key: CreatedInstallKey) => void;
}) {
  const titleId = useId();
  const multiCustomer = useMultiCustomer();
  const [siteId, setSiteId] = useState("");
  const [ttl, setTtl] = useState(30 * DAY);
  const [limit, setLimit] = useState("");
  const [approval, setApproval] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const max = Number.parseInt(limit, 10);
      onCreated(
        await api.createInstallKey({
          site_id: siteId || (sites[0]?.id ?? ""),
          ttl_s: ttl,
          ...(max > 0 ? { max_hosts: max } : {}),
          require_approval: approval,
        }),
      );
      setLimit("");
    } catch (e) {
      setError(t("install.key.createFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("install.key.title")}
      </h2>
      <div className={styles.formGrid}>
        <label className={styles.field}>
          {t("install.key.site")}
          <select className={fieldClass} value={siteId || sites[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {placeLabel(site.customer_name, site.name, multiCustomer)}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          {t("install.key.worksFor")}
          <select className={fieldClass} value={ttl} onChange={(e) => setTtl(Number(e.target.value))}>
            {VALIDITY.map(([seconds, label]) => (
              <option key={seconds} value={seconds}>
                {t(label)}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          <span>
            {t("install.key.upTo")} <span className={styles.fieldNote}>{t("install.key.upToNote")}</span>
          </span>
          <input
            className={fieldClass}
            type="number"
            min={1}
            max={100_000}
            value={limit}
            onChange={(e) => setLimit(e.target.value)}
            placeholder={t("install.key.anyNumber")}
          />
        </label>
      </div>
      <Switch checked={approval} onChange={setApproval} label={t("install.key.approval")} />
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || sites.length === 0}>
          {t("install.key.create")}
        </Button>
      </div>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

/** Hosts enrolled by a key that asks for approval: they connect once approved. */
export function PendingHosts({ hosts, now, onChange }: { hosts: PendingHost[]; now: Date; onChange: () => void }) {
  const titleId = useId();
  const [error, setError] = useState<string | null>(null);
  if (hosts.length === 0) return null;

  async function approve(host: PendingHost) {
    setError(null);
    try {
      await api.decidePendingHost(host.peer_id, "approve");
      onChange();
    } catch (e) {
      setError(t("install.pending.approveFailed", { name: host.hostname, error: describeError(e) }));
    }
  }

  const columns: Column<PendingHost>[] = [
    {
      key: "host",
      header: t("install.pending.column.server"),
      cell: (host) => (
        <>
          <span className={styles.peer}>{host.hostname}</span>
          {host.peer_id !== host.hostname && (
            <span className={styles.dim}>{t("install.pending.named", { name: host.peer_id })}</span>
          )}
        </>
      ),
    },
    { key: "site", header: t("install.pending.column.site"), cell: (host) => host.site_name },
    { key: "source", header: t("install.pending.column.from"), cell: (host) => (host.source ? <span className={styles.mono}>{host.source}</span> : <Missing word="unknown" className={styles.dim} />) },
    { key: "when", header: t("install.pending.column.installed"), cell: (host) => formatWhen(new Date(host.created_at), now) },
  ];
  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.pending.title")}
        </h2>
        <span className={styles.dim}>{hosts.length}</span>
      </div>
      <p className={styles.sectionNote}>{t("install.pending.note")}</p>
      <DataTable
        bare
        label={t("install.pending.label")}
        columns={columns}
        rows={hosts}
        rowKey={(host) => host.peer_id}
        rowActions={(host) => (
          <div className={styles.buttons}>
            <Button variant="k" onClick={() => void approve(host)} aria-label={t("install.pending.approveName", { name: host.hostname })}>
              {t("install.pending.approve")}
            </Button>
            <InlineConfirm
              layout="row"
              trigger={t("install.pending.reject")}
              triggerVariant="default"
              triggerName={t("install.pending.rejectName", { name: host.hostname })}
              title={t("install.pending.rejectTitle")}
              consequence={t("install.pending.rejectConsequence", { name: host.peer_id })}
              confirmLabel={t("install.pending.rejectConfirm")}
              pendingLabel={t("install.pending.rejecting")}
              onConfirm={async () => {
                await api.decidePendingHost(host.peer_id, "reject");
                onChange();
              }}
              describeError={(e) => t("install.pending.rejectFailed", { error: describeError(e) })}
            />
          </div>
        )}
      />
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </section>
  );
}

/** The install commands created so far, without their secret; an active one can be revoked. */
export function InstallKeyTable({
  keys,
  now,
  onChange,
}: {
  keys: InstallKey[] | undefined;
  now: Date;
  onChange: () => void;
}) {
  const titleId = useId();
  const multiCustomer = useMultiCustomer();
  const columns: Column<InstallKey>[] = [
    { key: "site", header: t("install.keys.column.site"), cell: (key) => placeLabel(key.customer_name, key.site_name, multiCustomer) },
    {
      key: "hosts",
      header: t("install.keys.column.installedOn"),
      cell: (key) => (
        <>
          {hostsWords(key)}
          {key.role === "relay" && <span className={styles.dim}>{t("install.keys.relay")}</span>}
          {key.require_approval && <span className={styles.dim}>{t("install.keys.withApproval")}</span>}
        </>
      ),
    },
    { key: "state", header: t("install.keys.column.state"), cell: (key) => <StatusLabel kind="installKey" value={key.state} /> },
    {
      key: "created",
      header: t("install.keys.column.created"),
      cell: (key) => (
        <>
          {formatWhen(new Date(key.created_at), now)}{" "}
          <span className={styles.dim}>
            {t("install.tokens.by", { name: key.created_by_username ?? t("install.tokens.byUnknown") })}
          </span>
        </>
      ),
    },
    {
      key: "expires",
      header: t("install.keys.column.expires"),
      cell: (key) => {
        const expires = new Date(key.expires_at);
        return key.state === "active" ? (
          <>
            {formatWhen(expires, now)} <span className={styles.dim}>· {formatAgo(expires, now)}</span>
          </>
        ) : (
          formatWhen(expires, now)
        );
      },
    },
  ];
  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("install.keys.title")}
        </h2>
        <span className={styles.dim}>
          {t("install.keys.active", { count: keys?.filter((key) => key.state === "active").length ?? 0 })}
        </span>
      </div>
      {keys && keys.length === 0 ? (
        <p className={styles.sectionNote}>{t("install.keys.none")}</p>
      ) : (
        <DataTable
          bare
          label={t("install.keys.title")}
          columns={columns}
          rows={keys ?? []}
          rowKey={(key) => key.id}
          rowMuted={(key) => key.state !== "active"}
          loading={!keys}
          rowActions={(key) =>
            key.state === "active" ? (
              <InlineConfirm
                layout="row"
                trigger={t("install.tokens.revoke")}
                triggerVariant="default"
                triggerName={t("install.keys.revokeName", { site: key.site_name })}
                title={t("install.keys.revokeTitle")}
                consequence={t("install.keys.revokeConsequence")}
                confirmLabel={t("install.keys.revokeConfirm")}
                pendingLabel={t("install.revoking")}
                onConfirm={async () => {
                  await api.revokeInstallKey(key.id);
                  onChange();
                }}
                describeError={(e) => t("install.keys.revokeFailed", { error: describeError(e) })}
              />
            ) : null
          }
        />
      )}
    </section>
  );
}
