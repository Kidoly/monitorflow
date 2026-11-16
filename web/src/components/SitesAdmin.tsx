"use client";

import Link from "next/link";
import { type FormEvent, useId, useState } from "react";

import { Button, fieldClass } from "@/components/ui/Button";
import { rich, t } from "@/i18n";
import { cx } from "@/components/ui/cx";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Label } from "@/components/ui/StatusLabel";
import { Banner, ErrorState, LoadingBlock, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { ApiError, type Customer, type Me, type Relay, type Server, type Site, api, describeError } from "@/lib/api";
import { administersCustomer, administersEverything, multiCustomer, useRefreshMe } from "@/lib/me";
import { AUTO_UPDATE_PATH, siteHref } from "@/lib/routes";
import { scopedHref } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import { OfflineAlerts } from "./OfflineAlerts";
import styles from "./AdminPages.module.css";

/** What the design asks of new names; the site name labels every stored metric. */
const SITE_NAME = /^[a-z0-9-]+$/;
const MAX_NAME = 64;

/**
 * `/settings/sites/`: sites grouped by customer, with a create form per
 * administered customer. Administrators of everything also create and
 * rename customers. Without customers to show (ADR-0104) it lists the
 * sites alone, and adding a first other customer is a quiet section at
 * the bottom.
 */
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
  const multi = multiCustomer(me);

  return (
    <div className={styles.page}>
      <PageHeader
        title={t("settings.sites.label")}
        meta={
          data || error ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : undefined
        }
      />
      <Banner tone="info">
        {multi ? t("sites.permanentCustomers") : t("sites.permanent")}
      </Banner>
      <Banner tone="info">
        {rich("sites.autoUpdateNote", { link: <Link href={AUTO_UPDATE_PATH}>{t("sites.autoUpdateLink")}</Link> })}
      </Banner>
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!data && error ? <ErrorState what={t("sites.what")} error={describeError(error)} onRetry={refresh} /> : null}
      {!data && !error && <LoadingBlock label={t("sites.loading")} />}
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
          canRename={multi && administersEverything(me)}
          multiCustomer={multi}
          onCreated={refresh}
          onChanged={refresh}
        />
      ))}
      {data &&
        administersEverything(me) &&
        (multi ? <CreateCustomer onCreated={refresh} /> : <SeveralCustomers own={data.customers[0]} onCreated={refresh} />)}
    </div>
  );
}

function CustomerSites({
  customer,
  rows,
  canCreate,
  canRename,
  multiCustomer,
  onCreated,
  onChanged,
}: {
  customer: Customer;
  rows: Row[];
  canCreate: boolean;
  canRename: boolean;
  multiCustomer: boolean;
  onCreated: () => void;
  onChanged: () => void;
}) {
  const titleId = useId();
  const columns: Column<Row>[] = [
    {
      key: "site",
      header: t("sites.column.site"),
      cell: (site) => (
        <PrimaryCell
          title={site.name}
          href={scopedHref("/", { customerId: site.customer_id, siteId: site.id })}
          sub={<Link href={siteHref(site.id)}>{t("sites.alertsAndChecks")}</Link>}
        />
      ),
    },
    {
      key: "servers",
      header: t("sites.column.servers"),
      cell: (site) => (
        <>
          <strong>{site.server_count}</strong>{" "}
          <span className={styles.dim}>· {t("sites.online", { count: site.online })}</span>
        </>
      ),
    },
    {
      key: "relays",
      header: t("sites.column.relays"),
      cell: (site) => (
        <>
          <strong>{site.relay_count}</strong>
          {site.relayNames.length > 0 && <span className={styles.dim}> · {site.relayNames.join(", ")}</span>}
        </>
      ),
    },
    {
      key: "auto-update",
      header: t("sites.column.autoUpdate"),
      cell: (site) => <AutoUpdate site={site} canChange={canCreate} onChanged={onChanged} />,
    },
    {
      key: "offline-alerts",
      header: t("sites.column.offlineAlerts"),
      cell: (site) => <OfflineAlerts kind="sites" target={{ ...site, site_id: site.id }} onChanged={onChanged} />,
    },
  ];
  return (
    // Without customers the page title already names the list: no second heading.
    <section
      className={styles.section}
      aria-labelledby={multiCustomer ? titleId : undefined}
      aria-label={multiCustomer ? undefined : t("settings.sites.label")}
    >
      {multiCustomer && (
        <div className={styles.sectionHead}>
          <h2 id={titleId} className={styles.sectionTitle}>
            {customer.name}
          </h2>
          {canRename && <RenameCustomer customer={customer} onRenamed={onChanged} />}
        </div>
      )}
      {rows.length === 0 ? (
        <p className={styles.sectionNote}>{t("sites.none")}</p>
      ) : (
        <DataTable
          bare
          label={multiCustomer ? t("sites.sitesOf", { name: customer.name }) : t("settings.sites.label")}
          columns={columns}
          rows={rows}
          rowKey={(site) => site.id}
        />
      )}
      {canCreate && <CreateSite customer={customer} multiCustomer={multiCustomer} onCreated={onCreated} />}
    </section>
  );
}

/** The site's override, as the select names it. */
type Override = "inherit" | "on" | "off";

const overrideOf = (value: boolean | null): Override => (value === null ? "inherit" : value ? "on" : "off");

/**
 * Whether the server updates the site's agents and relays automatically
 * (ADR-0055): the server-wide setting unless the site overrides it, with a
 * select for its customer's administrators.
 */
function AutoUpdate({ site, canChange, onChanged }: { site: Row; canChange: boolean; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const current = overrideOf(site.agent_auto_update);
  // Without the select, what applies now; the select already says it otherwise.
  if (!canChange) {
    return (
      <Label tone={site.agent_auto_update_effective ? "ok" : "neutral"}>
        {site.agent_auto_update_effective ? t("sites.automatic") : t("sites.manual")}
      </Label>
    );
  }
  async function change(next: Override) {
    setBusy(true);
    setError(null);
    try {
      await api.setAgentAutoUpdate(site.id, next === "inherit" ? null : next === "on");
      onChanged();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <select
        className={fieldClass}
        value={current}
        disabled={busy}
        onChange={(e) => void change(e.target.value as Override)}
        aria-label={t("sites.autoUpdateFor", { name: site.name })}
      >
        <option value="inherit">
          {current === "inherit"
            ? site.agent_auto_update_effective
              ? t("sites.serverDefaultOn")
              : t("sites.serverDefaultOff")
            : t("sites.serverDefault")}
        </option>
        <option value="on">{t("sites.on")}</option>
        <option value="off">{t("sites.off")}</option>
      </select>
      {error && <span role="alert"> {error}</span>}
    </>
  );
}

function CreateSite({
  customer,
  multiCustomer,
  onCreated,
}: {
  customer: Customer;
  multiCustomer: boolean;
  onCreated: () => void;
}) {
  const hintId = useId();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const wanted = name.trim();
    if (!SITE_NAME.test(wanted)) {
      setMessage({ error: true, text: t("sites.badName") });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      const site = await api.createSite(customer.id, wanted);
      setName("");
      setMessage({ error: false, text: t("sites.created", { name: site.name }) });
      onCreated();
    } catch (e) {
      setMessage({
        error: true,
        text:
          e instanceof ApiError && e.status === 409
            ? t("sites.exists", { name: wanted })
            : t("sites.createFailed", { error: describeError(e) }),
      });
    } finally {
      setBusy(false);
    }
  }

  const invalid = message?.error ?? false;
  return (
    <form
      className={styles.newSite}
      onSubmit={submit}
      aria-label={multiCustomer ? t("sites.newSiteFor", { name: customer.name }) : t("sites.newSite")}
      noValidate
    >
      <label className={styles.field}>
        {t("sites.newSiteName")}
        <input
          className={cx(fieldClass, styles.siteName, invalid && styles.fieldError)}
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder={t("sites.namePlaceholder")}
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
          {t("sites.nameHint")}
        </span>
      )}
      <Button variant="k" type="submit" disabled={busy || name.trim() === ""}>
        {t("sites.create")}
      </Button>
    </form>
  );
}

/** Turns the customer's name into a field with Save and Cancel. */
function RenameCustomer({ customer, onRenamed }: { customer: Customer; onRenamed: () => void }) {
  const [name, setName] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  if (name === null) {
    return (
      <Button size="small" className={styles.push} aria-label={t("sites.renameName", { name: customer.name })}
        onClick={() => setName(customer.name)}
      >
        {t("sites.rename")}
      </Button>
    );
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const wanted = (name ?? "").trim();
    setBusy(true);
    setError(null);
    try {
      await api.renameCustomer(customer.id, wanted);
      setName(null);
      onRenamed();
    } catch (e) {
      setError(
        e instanceof ApiError && e.status === 409
          ? t("sites.customerExists", { name: wanted })
          : t("sites.renameFailed", { error: describeError(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={cx(styles.buttons, styles.push)} onSubmit={submit} aria-label={t("sites.renameName", { name: customer.name })}>
      <input
        className={fieldClass}
        value={name}
        onChange={(e) => setName(e.target.value)}
        maxLength={MAX_NAME}
        aria-label={t("sites.newName")}
        autoFocus
      />
      <Button size="small" variant="k" type="submit" disabled={busy || name.trim() === ""}>
        {t("sites.save")}
      </Button>
      <Button size="small" onClick={() => setName(null)} disabled={busy}>
        {t("sites.cancel")}
      </Button>
      {error && (
        <span className={styles.hintError} role="alert">
          {error}
        </span>
      )}
    </form>
  );
}

/**
 * For a single company: where an MSP finds customers, folded away. The
 * first other customer shows customers everywhere (ADR-0104).
 */
function SeveralCustomers({ own, onCreated }: { own: Customer | undefined; onCreated: () => void }) {
  const titleId = useId();
  const [open, setOpen] = useState(false);
  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("sites.severalCustomers")}
        </h2>
        <Button size="small" aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? t("sites.hide") : t("sites.addCustomer")}
        </Button>
      </div>
      <p className={styles.sectionNote}>{t("sites.severalCustomersNote")}</p>
      {open && <CreateCustomer own={own} onCreated={onCreated} />}
    </section>
  );
}

/** Customers group sites for MSPs: each one's users can be limited to it. */
function CreateCustomer({ own, onCreated }: { own?: Customer; onCreated: () => void }) {
  const titleId = useId();
  const refreshMe = useRefreshMe();
  const [name, setName] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ error: boolean; text: string } | null>(null);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const wanted = name.trim();
    setBusy(true);
    setMessage(null);
    try {
      const customer = await api.createCustomer(wanted);
      setName("");
      setMessage({
        error: false,
        text: own
          ? t("sites.customerCreatedOwn", { name: customer.name, own: own.name })
          : t("sites.customerCreated", { name: customer.name }),
      });
      onCreated();
      // A second customer shows customers everywhere.
      refreshMe();
    } catch (e) {
      setMessage({
        error: true,
        text:
          e instanceof ApiError && e.status === 409
            ? t("sites.customerExists", { name: wanted })
            : t("sites.createCustomerFailed", { error: describeError(e) }),
      });
    } finally {
      setBusy(false);
    }
  }

  const form = (
    <form className={styles.newSite} onSubmit={submit} aria-label={t("sites.newCustomer")} noValidate>
      <label className={styles.field}>
        {t("sites.customerName")}
        <input
          className={cx(fieldClass, styles.siteName)}
          value={name}
          onChange={(e) => setName(e.target.value)}
          placeholder={t("sites.customerPlaceholder")}
          maxLength={MAX_NAME}
          required
        />
      </label>
      {message && (
        <span className={message.error ? styles.hintError : styles.hint} role={message.error ? "alert" : "status"}>
          {message.text}
        </span>
      )}
      <Button variant="k" type="submit" disabled={busy || name.trim() === ""}>
        {t("sites.createCustomer")}
      </Button>
    </form>
  );
  if (own) return form;
  return (
    <section className={styles.section} aria-labelledby={titleId}>
      <div className={styles.sectionHead}>
        <h2 id={titleId} className={styles.sectionTitle}>
          {t("sites.newCustomer")}
        </h2>
        <span className={styles.dim}>{t("sites.newCustomerNote")}</span>
      </div>
      {form}
    </section>
  );
}
