"use client";

import { type FormEvent, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { formatAgo } from "@/components/ui/time";
import { type CreatedInstallKey, type InstallKey, type PendingHost, type Site, api, describeError } from "@/lib/api";
import { formatWhen } from "@/lib/clock";

import styles from "./AdminPages.module.css";

const DAY = 86_400;

/** Validity choices; the API takes one hour to one year, 30 days by default. */
const VALIDITY: [number, string][] = [
  [DAY, "1 day"],
  [7 * DAY, "7 days"],
  [30 * DAY, "30 days"],
  [90 * DAY, "90 days"],
  [365 * DAY, "1 year"],
];

/** "3 of 10 hosts", or "3 hosts" without a limit. */
export function hostsWords(key: Pick<InstallKey, "host_count" | "max_hosts">): string {
  const unit = (key.max_hosts ?? key.host_count) === 1 ? "host" : "hosts";
  return key.max_hosts === null ? `${key.host_count} ${unit}` : `${key.host_count} of ${key.max_hosts} ${unit}`;
}

/** The form that creates a site's install command (ADR-0058); every field has a default. */
export function CreateInstallKey({
  sites,
  username,
  onCreated,
}: {
  sites: Site[];
  username: string;
  onCreated: (key: CreatedInstallKey) => void;
}) {
  const titleId = useId();
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
      setError(`Creating the install command failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={submit} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.sectionTitle}>
        Install command
      </h2>
      <p className={styles.sectionNote}>
        One command for all the hosts of a site, Linux or Windows. Each host appears under its own name.
      </p>
      <div className={styles.formGrid}>
        <label className={styles.field}>
          Site
          <select className={fieldClass} value={siteId || sites[0]?.id} onChange={(e) => setSiteId(e.target.value)}>
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {site.customer_name} › {site.name}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          Works for
          <select className={fieldClass} value={ttl} onChange={(e) => setTtl(Number(e.target.value))}>
            {VALIDITY.map(([seconds, label]) => (
              <option key={seconds} value={seconds}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          <span>
            Up to <span className={styles.fieldNote}>— hosts; empty for any number</span>
          </span>
          <input
            className={fieldClass}
            type="number"
            min={1}
            max={100_000}
            value={limit}
            onChange={(e) => setLimit(e.target.value)}
            placeholder="any number"
          />
        </label>
      </div>
      <label className={styles.check}>
        <input type="checkbox" checked={approval} onChange={(e) => setApproval(e.target.checked)} /> Approve new hosts
        before they appear
      </label>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy || sites.length === 0}>
          Create install command
        </Button>
        <span className={styles.footnote}>Recorded in the audit log as {username}.</span>
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
      setError(`Approving ${host.hostname} failed. ${describeError(e)}`);
    }
  }

  const columns: Column<PendingHost>[] = [
    {
      key: "host",
      header: "Host",
      cell: (host) => (
        <>
          <span className={styles.peer}>{host.hostname}</span>
          {host.peer_id !== host.hostname && <span className={styles.dim}> · named {host.peer_id}</span>}
        </>
      ),
    },
    { key: "site", header: "Site", cell: (host) => host.site_name },
    { key: "source", header: "From", cell: (host) => <span className={styles.mono}>{host.source ?? "—"}</span> },
    { key: "when", header: "Installed", cell: (host) => formatWhen(new Date(host.created_at), now) },
  ];
  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          Waiting for approval
        </h2>
        <span className={styles.dim}>{hosts.length}</span>
      </div>
      <p className={styles.sectionNote}>
        These hosts ran an install command that asks for approval. They appear under Servers once approved.
      </p>
      <DataTable
        bare
        label="Hosts waiting for approval"
        columns={columns}
        rows={hosts}
        rowKey={(host) => host.peer_id}
        rowActions={(host) => (
          <div className={styles.buttons}>
            <Button variant="k" onClick={() => void approve(host)} aria-label={`Approve ${host.hostname}`}>
              Approve
            </Button>
            <InlineConfirm
              layout="row"
              trigger="Reject"
              triggerVariant="default"
              triggerName={`Reject ${host.hostname}`}
              title="Reject this host?"
              consequence={`Its certificate is revoked: it can never connect as ${host.peer_id}.`}
              confirmLabel="Reject host"
              pendingLabel="Rejecting…"
              onConfirm={async () => {
                await api.decidePendingHost(host.peer_id, "reject");
                onChange();
              }}
              describeError={(e) => `Rejecting the host failed. ${describeError(e)}`}
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
  const columns: Column<InstallKey>[] = [
    { key: "site", header: "Site", cell: (key) => `${key.customer_name} › ${key.site_name}` },
    {
      key: "hosts",
      header: "Installed on",
      cell: (key) => (
        <>
          {hostsWords(key)}
          {key.require_approval && <span className={styles.dim}> · with approval</span>}
        </>
      ),
    },
    { key: "state", header: "State", cell: (key) => <StatusLabel kind="installKey" value={key.state} /> },
    {
      key: "created",
      header: "Created",
      cell: (key) => (
        <>
          {formatWhen(new Date(key.created_at), now)}{" "}
          <span className={styles.dim}>by {key.created_by_username ?? "the server CLI"}</span>
        </>
      ),
    },
    {
      key: "expires",
      header: "Expires",
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
          Install commands
        </h2>
        <span className={styles.dim}>{keys?.filter((key) => key.state === "active").length ?? 0} active</span>
      </div>
      {keys && keys.length === 0 ? (
        <p className={styles.sectionNote}>None yet: create one above, then paste it on your hosts.</p>
      ) : (
        <DataTable
          bare
          label="Install commands"
          columns={columns}
          rows={keys ?? []}
          rowKey={(key) => key.id}
          rowMuted={(key) => key.state !== "active"}
          loading={!keys}
          rowActions={(key) =>
            key.state === "active" ? (
              <InlineConfirm
                layout="row"
                trigger="Revoke"
                triggerVariant="default"
                triggerName={`Revoke the install command of ${key.site_name}`}
                title="Revoke this install command?"
                consequence="No new host can install with it. Hosts already installed keep working."
                confirmLabel="Revoke command"
                pendingLabel="Revoking…"
                onConfirm={async () => {
                  await api.revokeInstallKey(key.id);
                  onChange();
                }}
                describeError={(e) => `Revoking the install command failed. ${describeError(e)}`}
              />
            ) : (
              <span className={styles.dim}>—</span>
            )
          }
        />
      )}
    </section>
  );
}
