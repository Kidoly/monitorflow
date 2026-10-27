"use client";

import { type FormEvent, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Banner, ErrorState, LoadingBlock, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { ApiError, type Customer, type Me, type Relay, type Server, type Site, api, describeError } from "@/lib/api";
import { administersCustomer } from "@/lib/me";
import { scopedHref } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/** What the design asks of new names; the site name labels every stored metric. */
const SITE_NAME = /^[a-z0-9-]+$/;
const MAX_NAME = 64;

/** `/admin/sites/`: sites grouped by customer, with a create form per administered customer. */
export function SitesAdminPage() {
  return <AdminLayout>{(me) => <SitesAdmin me={me} />}</AdminLayout>;
}

interface Data {
  customers: Customer[];
  sites: Site[];
  servers: Server[];
  relays: Relay[];
}

/** A site with its online servers and relay names, from the servers and relays lists. */
interface Row extends Site {
  online: number;
  relayNames: string[];
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

async function load(signal: AbortSignal): Promise<Data> {
  const [{ customers }, { sites }, { servers }, { relays }] = await Promise.all([
    api.customers(signal),
    api.sites(signal),
    api.servers(signal),
    api.relays(signal),
  ]);
  return { customers, sites, servers, relays };
}

export function SitesAdmin({ me }: { me: Me }) {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(load);

  return (
    <div className={styles.page}>
      <PageHeader
        title="Sites"
        meta={
          data || error ? (
            <>
              {data && `${plural(data.customers.length, "customer")}, ${plural(data.sites.length, "site")} · `}
              <RefreshLine updatedAt={updatedAt} stale={stale} />
            </>
          ) : undefined
        }
      />
      <Banner tone="info">
        Sites cannot be renamed or moved to another customer, because the site name labels every stored metric.
        Choose names that will still make sense in two years.
      </Banner>
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error ? <ErrorState what="sites" error={describeError(error)} onRetry={refresh} /> : null}
      {!data && !error && <LoadingBlock label="Loading sites" />}
      {data?.customers.map((customer) => (
        <CustomerSites
          key={customer.id}
          customer={customer}
          rows={data.sites
            .filter((site) => site.customer_id === customer.id)
            .map((site) => ({
              ...site,
              online: data.servers.filter((s) => s.site_id === site.id && !s.revoked_at && s.status === "online").length,
              relayNames: data.relays.filter((r) => r.site_id === site.id && !r.revoked_at).map((r) => r.peer_id),
            }))}
          canCreate={administersCustomer(me, customer.id)}
          onCreated={refresh}
          onChanged={refresh}
        />
      ))}
    </div>
  );
}

function CustomerSites({
  customer,
  rows,
  canCreate,
  onCreated,
  onChanged,
}: {
  customer: Customer;
  rows: Row[];
  canCreate: boolean;
  onCreated: () => void;
  onChanged: () => void;
}) {
  const titleId = useId();
  const servers = rows.reduce((n, row) => n + row.server_count, 0);
  const relays = rows.reduce((n, row) => n + row.relay_count, 0);
  const columns: Column<Row>[] = [
    {
      key: "site",
      header: "Site",
      cell: (site) => (
        <PrimaryCell title={site.name} href={scopedHref("/", { customerId: site.customer_id, siteId: site.id })} />
      ),
    },
    {
      key: "servers",
      header: "Servers",
      cell: (site) => (
        <>
          <strong>{site.server_count}</strong> <span className={styles.dim}>· {site.online} online</span>
        </>
      ),
    },
    {
      key: "relays",
      header: "Relays",
      cell: (site) => (
        <>
          <strong>{site.relay_count}</strong>
          {site.relayNames.length > 0 && <span className={styles.dim}> · {site.relayNames.join(", ")}</span>}
        </>
      ),
    },
    {
      key: "auto-update",
      header: "Agent updates",
      cell: (site) => <AutoUpdate site={site} canChange={canCreate} onChanged={onChanged} />,
    },
  ];
  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {customer.name}
        </h2>
        <span className={styles.dim}>
          {plural(rows.length, "site")} · {plural(servers, "server")} · {plural(relays, "relay")}
        </span>
      </div>
      {rows.length === 0 ? (
        <p className={styles.sectionNote}>No sites yet.</p>
      ) : (
        <DataTable bare label={`Sites of ${customer.name}`} columns={columns} rows={rows} rowKey={(site) => site.id} />
      )}
      {canCreate && <CreateSite customer={customer} onCreated={onCreated} />}
    </section>
  );
}

/**
 * Whether the server updates the site's agents to the newest release it
 * holds (ADR-0053), with a button for its customer's administrators.
 */
function AutoUpdate({ site, canChange, onChanged }: { site: Row; canChange: boolean; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const state = site.agent_auto_update ? "Automatic" : "Manual";
  if (!canChange) return <span className={styles.dim}>{state}</span>;
  async function toggle() {
    setBusy(true);
    setError(null);
    try {
      await api.setAgentAutoUpdate(site.id, !site.agent_auto_update);
      onChanged();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <span className={styles.dim}>{state}</span>{" "}
      <Button size="small" disabled={busy} onClick={() => void toggle()} aria-label={`${site.agent_auto_update ? "Stop" : "Start"} automatic agent updates for ${site.name}`}>
        {site.agent_auto_update ? "Turn off" : "Turn on"}
      </Button>
      {error && <span role="alert"> {error}</span>}
    </>
  );
}

function CreateSite({ customer, onCreated }: { customer: Customer; onCreated: () => void }) {
  const hintId = useId();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const wanted = name.trim();
    if (!SITE_NAME.test(wanted)) {
      setMessage({ error: true, text: "Use lowercase letters, digits and dashes only." });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const site = await api.createSite(customer.id, wanted);
      setName("");
      setMessage({ error: false, text: `Created site ${site.name}.` });
      onCreated();
    } catch (e) {
      setMessage({
        error: true,
        text:
          e instanceof ApiError && e.status === 409
            ? `A site named "${wanted}" already exists.`
            : `Creating the site failed. ${describeError(e)}`,
      });
    } finally {
      setBusy(false);
    }
  }

  const invalid = message?.error ?? false;
  return (
    <form className={styles.newSite} onSubmit={submit} aria-label={`New site for ${customer.name}`} noValidate>
      <label className={styles.field}>
        New site name
        <input
          className={cx(fieldClass, styles.siteName, invalid && styles.fieldError)}
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder="e.g. marseille-dc2"
          maxLength={MAX_NAME}
          required
          spellCheck={false}
          autoCapitalize="none"
          aria-invalid={invalid || undefined}
          aria-describedby={hintId}
        />
      </label>
      {message ? (
        <span
          id={hintId}
          className={message.error ? styles.hintError : styles.hint}
          role={message.error ? "alert" : "status"}
        >
          {message.text}
        </span>
      ) : (
        <span id={hintId} className={styles.hint}>
          Lowercase letters, digits and dashes. Permanent.
        </span>
      )}
      <Button variant="k" type="submit" disabled={busy || name.trim() === ""}>
        Create site
      </Button>
    </form>
  );
}
