"use client";

import { type FormEvent, useId, useState } from "react";

import {
  ApiError,
  type Customer,
  type InstalledPlugin,
  type PluginInspection,
  type PluginPublisher,
  type Site,
  api,
  describeError,
} from "@/lib/api";
import { customerWide, placeLabel } from "@/lib/customers";
import { administersEverything, useMe, useMultiCustomer } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { Button, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { InlineConfirm } from "./ui/InlineConfirm";
import { Banner, EmptyState, ErrorState, LoadingBlock, NoPermission, PageHeader } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";

const GUIDE = "https://github.com/Kidoly/monitorflow/blob/main/docs/plugins.md";

/** `/admin/plugins/`: global administrators only, like the API (ADR-0103). */
export function PluginsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title="Plugins need a global administrator">
        A plugin runs on the central server or on your servers, so only a global administrator installs one.
      </NoPermission>
    );
  }
  return <Plugins />;
}

const KIND_WORDS: Record<InstalledPlugin["kind"], string> = {
  check: "Check",
  channel: "Notification channel",
};

/** A place as the editor keys it: `everything`, `customer:ID` or `site:ID`. */
type PlaceKey = string;

/** One choice of where a plugin may be used. */
interface PlaceChoice {
  key: PlaceKey;
  label: string;
}

/**
 * Where a plugin may be used, widest first (ADR-0104): customers only for
 * someone who sees several (`multiCustomer`), sites once there are
 * several; with neither the choice is just on or off.
 */
export function placeChoices(customers: Customer[], sites: Site[], multiCustomer: boolean): PlaceChoice[] {
  const choices: PlaceChoice[] = [{ key: "everything", label: multiCustomer ? "Every customer" : "Every server" }];
  if (multiCustomer) {
    for (const c of customers) choices.push({ key: `customer:${c.id}`, label: `All of ${c.name}` });
  }
  if (multiCustomer || sites.length > 1) {
    for (const s of sites) {
      choices.push({ key: `site:${s.id}`, label: placeLabel(s.customer_name, s.name, multiCustomer, " / ") });
    }
  }
  return choices;
}

function placeKey(place: InstalledPlugin["places"][number]): PlaceKey {
  if (place.site_id) return `site:${place.site_id}`;
  return place.customer_id ? `customer:${place.customer_id}` : "everything";
}

/** Where a plugin may be used, in a few words. */
export function placesSummary(plugin: InstalledPlugin, multiCustomer: boolean): string {
  if (plugin.places.length === 0) return "Nowhere yet";
  if (plugin.places.some((p) => !p.customer_id && !p.site_id)) return "Everywhere";
  return plugin.places
    .map((p) =>
      p.site_id
        ? placeLabel(p.customer_name, p.site_name ?? "", multiCustomer, " / ")
        : customerWide(`All of ${p.customer_name}`, multiCustomer),
    )
    .join(", ");
}

function errorText(e: unknown): string {
  return e instanceof ApiError && e.status < 500 ? e.message : describeError(e);
}

function Plugins() {
  const { data, error, refresh } = usePolledData(api.plugins);
  const customers = usePolledData(api.customers, { intervalMs: 60_000 });
  const multi = useMultiCustomer();
  const sites = usePolledData(api.sites, { intervalMs: 60_000 });
  const [placing, setPlacing] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const header = <PageHeader title="Plugins" meta="Global administrators only" />;
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title="Plugins need a global administrator" />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what="the plugins" error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const allCustomers = customers.data?.customers ?? [];
  const allSites = sites.data?.sites ?? [];
  const choices = placeChoices(allCustomers, allSites, multi);
  const editing = data.plugins.find((p) => p.id === placing);

  async function act(action: () => Promise<unknown>) {
    setActionError(null);
    try {
      await action();
      refresh();
    } catch (e) {
      setActionError(errorText(e));
    }
  }

  const columns: Column<InstalledPlugin>[] = [
    {
      key: "plugin",
      header: "Plugin",
      className: styles.wrap,
      cell: (p) => (
        <>
          <strong>{p.title}</strong> {p.version}
          <div className={styles.dim}>
            {KIND_WORDS[p.kind]} · signed by {p.publisher ?? `a key no longer trusted (${p.fingerprint})`}
          </div>
        </>
      ),
    },
    {
      key: "asks",
      header: "Asks for",
      className: styles.wrap,
      cell: (p) => p.capabilities.slice(1).join(". "),
    },
    {
      key: "where",
      header: "Used where",
      className: styles.wrap,
      cell: (p) => placesSummary(p, multi),
    },
    {
      key: "status",
      header: "Status",
      cell: (p) =>
        p.enabled ? (
          <>
            <Label tone="ok">On</Label>{" "}
            <span className={styles.dim}>
              {p.kind === "check" ? `${p.checks} check(s)` : `${p.channels} channel(s)`}
            </span>
          </>
        ) : (
          <Label tone="neutral">Off</Label>
        ),
    },
    {
      key: "actions",
      header: "",
      cell: (p) => (
        <span className={styles.buttons}>
          <Button size="small" onClick={() => setPlacing(p.id)}>
            Where
          </Button>
          <Button size="small" onClick={() => void act(() => api.setPluginEnabled(p.id, !p.enabled))}>
            {p.enabled ? "Turn off" : "Turn on"}
          </Button>
          <InlineConfirm
            layout="row"
            trigger="Remove"
            triggerName={`Remove ${p.title}`}
            title={`Remove ${p.title}?`}
            consequence="It is deleted from the central server. Remove its checks and channels first."
            confirmLabel="Remove"
            onConfirm={async () => {
              await api.removePlugin(p.id);
              refresh();
            }}
            describeError={errorText}
          />
        </span>
      ),
    },
  ];

  return (
    <div className={styles.page}>
      {header}
      <p className={styles.footnote}>
        Plugins add checks and notification channels. Each is signed by its publisher and runs in a sandbox: it reads
        no files and runs no programs, and reaches only the addresses it shows here. To make your own, see{" "}
        <a href={GUIDE}>Write a plugin</a>.
      </p>
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <Upload
        onInstalled={(plugin, isNew) => {
          refresh();
          if (isNew) setPlacing(plugin.id);
        }}
      />
      {editing && (
        <PlacesForm
          key={editing.id}
          plugin={editing}
          choices={choices}
          onCancel={() => setPlacing(null)}
          onSaved={() => {
            setPlacing(null);
            refresh();
          }}
        />
      )}
      {data.plugins.length === 0 ? (
        <EmptyState title="No plugin installed">
          Upload a .mfplugin file above. Plugins from your own publishers need their key under Publishers first.
        </EmptyState>
      ) : (
        <section className={styles.section} aria-label="Installed plugins">
          <DataTable bare label="Installed plugins" columns={columns} rows={data.plugins} rowKey={(p) => p.id} />
        </section>
      )}
      <Publishers />
    </div>
  );
}

/** Upload, read what the file asks for, then install it. */
function Upload({ onInstalled }: { onInstalled: (plugin: InstalledPlugin, isNew: boolean) => void }) {
  const [file, setFile] = useState<File | null>(null);
  const [shown, setShown] = useState<PluginInspection | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();

  async function choose(chosen: File | null) {
    setFile(chosen);
    setShown(null);
    setError(null);
    if (!chosen) return;
    setBusy(true);
    try {
      setShown(await api.inspectPlugin(chosen));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  async function install() {
    if (!file || !shown) return;
    setBusy(true);
    setError(null);
    try {
      const plugin = await api.installPlugin(file, shown.sha256);
      setFile(null);
      setShown(null);
      onInstalled(plugin, shown.update === null);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className={styles.panel} aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`} className={styles.sectionTitle}>
        Install a plugin
      </h2>
      <label className={styles.field}>
        Plugin file
        <input
          key={file ? "chosen" : "empty"}
          type="file"
          accept=".mfplugin"
          disabled={busy}
          onChange={(e) => void choose(e.target.files?.[0] ?? null)}
        />
        <span className={styles.fieldNote}>Nothing is installed before you read what it asks for.</span>
      </label>
      {busy && !shown && <p className={styles.dim}>Checking its signature…</p>}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {shown && (
        <>
          <h3 className={styles.sectionTitle}>
            {shown.title} {shown.version}
            {shown.update && <span className={styles.dim}> (installed: {shown.update.from_version})</span>}
          </h3>
          {shown.description && <p>{shown.description}</p>}
          <p className={styles.dim}>
            Signed by {shown.official ? "MonitorFlow" : shown.publisher}, key {shown.fingerprint}
          </p>
          <ul aria-label="What it asks for">
            {(shown.update ? shown.update.adds : shown.capabilities).map((c) => (
              <li key={c}>{c}</li>
            ))}
          </ul>
          {shown.update && shown.update.adds.length === 0 && (
            <p className={styles.dim}>This version asks for nothing new.</p>
          )}
          {shown.update && shown.update.drops.length > 0 && (
            <p className={styles.dim}>It no longer asks to: {shown.update.drops.join(", ")}.</p>
          )}
          <div className={styles.formFoot}>
            <Button variant="k" disabled={busy} onClick={() => void install()}>
              {busy ? "Installing…" : shown.update ? `Update to ${shown.version}` : "Install"}
            </Button>
            <Button disabled={busy} onClick={() => void choose(null)}>
              Cancel
            </Button>
          </div>
        </>
      )}
    </section>
  );
}

function PlacesForm({
  plugin,
  choices,
  onCancel,
  onSaved,
}: {
  plugin: InstalledPlugin;
  choices: PlaceChoice[];
  onCancel: () => void;
  onSaved: () => void;
}) {
  const [chosen, setChosen] = useState<Set<PlaceKey>>(() => new Set(plugin.places.map(placeKey)));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const everything = chosen.has("everything");

  function toggle(key: PlaceKey, on: boolean) {
    setChosen((current) => {
      const next = new Set(current);
      if (on) next.add(key);
      else next.delete(key);
      return next;
    });
  }

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const places = everything
      ? [{}]
      : [...chosen].map((key) => {
          const [kind, value] = key.split(":");
          return kind === "site" ? { site_id: value } : { customer_id: value };
        });
    try {
      await api.setPluginPlaces(plugin.id, places);
      onSaved();
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  }

  const use = plugin.kind === "check" ? "as a check" : "as a notification channel";
  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`} className={styles.sectionTitle}>
        Where {plugin.title} may be used
      </h2>
      <p className={styles.dim}>
        Where it is allowed, administrators can add it {use}. Nothing runs until they do.
      </p>
      {choices.map((choice) => (
        <label key={choice.key}>
          <input
            type="checkbox"
            checked={chosen.has(choice.key)}
            disabled={choice.key !== "everything" && everything}
            onChange={(e) => toggle(choice.key, e.target.checked)}
          />{" "}
          {choice.label}
        </label>
      ))}
      <div className={styles.formFoot}>
        <Button type="submit" variant="k" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        {error && (
          <p className={styles.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}

function Publishers() {
  const { data, error, refresh } = usePolledData(api.pluginPublishers, { intervalMs: 60_000 });
  const [name, setName] = useState("");
  const [key, setKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const id = useId();

  async function add(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setFormError(null);
    try {
      await api.addPluginPublisher(name, key);
      setName("");
      setKey("");
      refresh();
    } catch (e) {
      setFormError(errorText(e));
    } finally {
      setBusy(false);
    }
  }

  const columns: Column<PluginPublisher>[] = [
    { key: "name", header: "Publisher", cell: (p) => <strong>{p.name}</strong> },
    { key: "key", header: "Key", cell: (p) => <span className={styles.mono}>{p.fingerprint}</span> },
    {
      key: "plugins",
      header: "Installed plugins",
      className: styles.wrap,
      cell: (p) => (p.plugins.length ? p.plugins.join(", ") : <span className={styles.dim}>none</span>),
    },
    {
      key: "actions",
      header: "",
      cell: (p) => (
        <InlineConfirm
          layout="row"
          trigger="Remove"
          triggerName={`Remove ${p.name}`}
          title={`Stop trusting ${p.name}?`}
          consequence="Their plugins can no longer be installed or updated. Remove their installed plugins first."
          confirmLabel="Remove"
          onConfirm={async () => {
            await api.removePluginPublisher(p.id);
            refresh();
          }}
          describeError={errorText}
        />
      ),
    },
  ];

  return (
    <section className={styles.section} aria-labelledby={`${id}-title`}>
      <div className={styles.sectionHead}>
        <h2 id={`${id}-title`} className={styles.sectionTitle}>
          Publishers
        </h2>
      </div>
      <p className={styles.sectionNote}>
        Plugins signed by MonitorFlow install as they are. Add the key of anyone else whose plugins you trust,
        your own team included: they print it with <code>monitorflow-plugin keygen</code>.
      </p>
      {error !== null && !data && <ErrorState what="the publishers" error={describeError(error)} onRetry={refresh} />}
      {data && data.publishers.length > 0 && (
        <DataTable bare label="Publishers" columns={columns} rows={data.publishers} rowKey={(p) => p.id} />
      )}
      <form className={styles.panel} onSubmit={(e) => void add(e)} aria-label="Add a publisher">
        <div className={styles.formGrid}>
          <label className={styles.field}>
            Name
            <input
              className={fieldClass}
              required
              maxLength={80}
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Acme IT"
            />
          </label>
          <label className={styles.field}>
            Public key
            <input
              className={fieldClass}
              required
              spellCheck={false}
              autoComplete="off"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder="The line monitorflow-plugin keygen printed"
            />
          </label>
        </div>
        <div className={styles.formFoot}>
          <Button type="submit" disabled={busy}>
            {busy ? "Adding…" : "Trust this publisher"}
          </Button>
          {formError && (
            <p className={styles.formError} role="alert">
              {formError}
            </p>
          )}
        </div>
      </form>
    </section>
  );
}
