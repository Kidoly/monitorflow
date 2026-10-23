"use client";

import { type FormEvent, useCallback, useState } from "react";

import { type Customer, type Me, type Site, api, describeError, isAbortError } from "@/lib/api";
import { administersCustomer } from "@/lib/me";
import { usePolling } from "@/lib/usePolling";

import styles from "./Admin.module.css";
import { AdminLayout } from "./AdminNav";
import { REFRESH_MS } from "./FleetOverview";

/** `/admin/sites/`: sites grouped by customer, with a create form per administered customer. */
export function SitesAdminPage() {
  return <AdminLayout>{(me) => <SitesAdmin me={me} />}</AdminLayout>;
}

interface Data {
  customers: Customer[];
  sites: Site[];
}

export function SitesAdmin({ me }: { me: Me }) {
  const [data, setData] = useState<Data | null>(null);
  const [error, setError] = useState<string | null>(null);
  const load = useCallback(async (signal?: AbortSignal) => {
    try {
      const [{ customers }, { sites }] = await Promise.all([api.customers(signal), api.sites(signal)]);
      setData({ customers, sites });
      setError(null);
    } catch (e) {
      if (!isAbortError(e)) setError(describeError(e));
    }
  }, []);
  usePolling(load, REFRESH_MS);

  return (
    <section aria-labelledby="sites-title">
      <h1 id="sites-title">Sites</h1>
      <p className={styles.note}>
        Sites cannot be renamed or moved to another customer: the site name labels every stored metric.
      </p>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      {!data && !error && <p role="status">Loading sites…</p>}
      {data?.customers.map((customer) => (
        <section key={customer.id} className={styles.customer} aria-labelledby={`customer-${customer.id}`}>
          <h2 id={`customer-${customer.id}`}>{customer.name}</h2>
          <SiteTable sites={data.sites.filter((s) => s.customer_id === customer.id)} />
          {administersCustomer(me, customer.id) && (
            <CreateSite customer={customer} onCreated={() => void load()} />
          )}
        </section>
      ))}
    </section>
  );
}

function SiteTable({ sites }: { sites: Site[] }) {
  if (sites.length === 0) return <p className={styles.meta}>No sites you can see.</p>;
  return (
    <table className={styles.table}>
      <thead>
        <tr>
          <th scope="col">Site</th>
          <th scope="col">Servers</th>
          <th scope="col">Relays</th>
        </tr>
      </thead>
      <tbody>
        {sites.map((site) => (
          <tr key={site.id}>
            <th scope="row">{site.name}</th>
            <td>{site.server_count}</td>
            <td>{site.relay_count}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function CreateSite({ customer, onCreated }: { customer: Customer; onCreated: () => void }) {
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setMessage(null);
    try {
      const site = await api.createSite(customer.id, name.trim());
      setName("");
      setMessage({ error: false, text: `Created site ${site.name}.` });
      onCreated();
    } catch (e) {
      setMessage({ error: true, text: `Creating the site failed. ${describeError(e)}` });
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} onSubmit={submit} aria-label={`New site for ${customer.name}`}>
      <label>
        New site name
        <input value={name} onChange={(e) => setName(e.target.value)} required maxLength={64} />
      </label>
      <button className={styles.button} type="submit" disabled={busy || name.trim() === ""}>
        Create site
      </button>
      {message && (
        <p className={message.error ? styles.error : styles.success} role={message.error ? "alert" : "status"}>
          {message.text}
        </p>
      )}
    </form>
  );
}
