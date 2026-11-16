"use client";

import { type FormEvent, useId, useState } from "react";

import { rich, t } from "@/i18n";
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

/** `/settings/plugins/`: global administrators only, like the API (ADR-0103). */
export function PluginsPage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("plugins.needAdmin")}>{t("plugins.needAdminBody")}</NoPermission>
    );
  }
  return <Plugins />;
}

const kindWord = (kind: InstalledPlugin["kind"]) => t(`plugins.kind.${kind}`);

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
  const choices: PlaceChoice[] = [{ key: "everything", label: multiCustomer ? t("plugins.everyCustomer") : t("plugins.everyServer") }];
  if (multiCustomer) {
    for (const c of customers) choices.push({ key: `customer:${c.id}`, label: t("plugins.allOf", { name: c.name }) });
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
  if (plugin.places.length === 0) return t("plugins.nowhere");
  if (plugin.places.some((p) => !p.customer_id && !p.site_id)) return t("plugins.everywhere");
  return plugin.places
    .map((p) =>
      p.site_id
        ? placeLabel(p.customer_name, p.site_name ?? "", multiCustomer, " / ")
        : customerWide(t("plugins.allOf", { name: p.customer_name ?? "" }), multiCustomer),
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

  const header = <PageHeader title={t("plugins.title")} meta={t("plugins.meta")} />;
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("plugins.needAdmin")} />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("plugins.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
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
      header: t("plugins.column.plugin"),
      className: styles.wrap,
      cell: (p) => (
        <>
          <strong>{p.title}</strong> {p.version}
          <div className={styles.dim}>
            {t("plugins.signedBy", {
              kind: kindWord(p.kind),
              publisher: p.publisher ?? t("plugins.untrustedKey", { fingerprint: p.fingerprint }),
            })}
          </div>
        </>
      ),
    },
    {
      key: "asks",
      header: t("plugins.column.asks"),
      className: styles.wrap,
      cell: (p) => p.capabilities.slice(1).join(". "),
    },
    {
      key: "where",
      header: t("plugins.column.where"),
      className: styles.wrap,
      cell: (p) => placesSummary(p, multi),
    },
    {
      key: "status",
      header: t("plugins.column.status"),
      cell: (p) =>
        p.enabled ? (
          <>
            <Label tone="ok">{t("plugins.on")}</Label>{" "}
            <span className={styles.dim}>
              {p.kind === "check" ? t("plugins.checks", { count: p.checks }) : t("plugins.channels", { count: p.channels })}
            </span>
          </>
        ) : (
          <Label tone="neutral">{t("plugins.off")}</Label>
        ),
    },
    {
      key: "actions",
      header: "",
      cell: (p) => (
        <span className={styles.buttons}>
          <Button size="small" onClick={() => setPlacing(p.id)}>
            {t("plugins.where")}
          </Button>
          <Button size="small" onClick={() => void act(() => api.setPluginEnabled(p.id, !p.enabled))}>
            {p.enabled ? t("plugins.turnOff") : t("plugins.turnOn")}
          </Button>
          <InlineConfirm
            layout="row"
            trigger={t("plugins.remove")}
            triggerName={t("plugins.removeName", { name: p.title })}
            title={t("plugins.removeTitle", { name: p.title })}
            consequence={t("plugins.removeConsequence")}
            confirmLabel={t("plugins.remove")}
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
        {rich("plugins.intro", { guide: <a href={GUIDE}>{t("plugins.guide")}</a> })}
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
        <EmptyState title={t("plugins.emptyTitle")} />
      ) : (
        <section className={styles.section} aria-label={t("plugins.installed")}>
          <DataTable bare label={t("plugins.installed")} columns={columns} rows={data.plugins} rowKey={(p) => p.id} />
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
        {t("plugins.install")}
      </h2>
      <label className={styles.field}>
        {t("plugins.file")}
        <input
          key={file ? "chosen" : "empty"}
          type="file"
          accept=".mfplugin"
          disabled={busy}
          onChange={(e) => void choose(e.target.files?.[0] ?? null)}
        />
        <span className={styles.fieldNote}>{t("plugins.fileNote")}</span>
      </label>
      {busy && !shown && <p className={styles.dim}>{t("plugins.checking")}</p>}
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
      {shown && (
        <>
          <h3 className={styles.sectionTitle}>
            {shown.title} {shown.version}
            {shown.update && <span className={styles.dim}> {t("plugins.installedVersion", { version: shown.update.from_version })}</span>}
          </h3>
          {shown.description && <p>{shown.description}</p>}
          <p className={styles.dim}>
            {t("plugins.signedByKey", {
              publisher: shown.official ? "MonitorFlow" : shown.publisher,
              fingerprint: shown.fingerprint,
            })}
          </p>
          <ul aria-label={t("plugins.asks")}>
            {(shown.update ? shown.update.adds : shown.capabilities).map((c) => (
              <li key={c}>{c}</li>
            ))}
          </ul>
          {shown.update && shown.update.adds.length === 0 && (
            <p className={styles.dim}>{t("plugins.nothingNew")}</p>
          )}
          {shown.update && shown.update.drops.length > 0 && (
            <p className={styles.dim}>{t("plugins.drops", { drops: shown.update.drops.join(", ") })}</p>
          )}
          <div className={styles.formFoot}>
            <Button variant="k" disabled={busy} onClick={() => void install()}>
              {busy
                ? t("plugins.installing")
                : shown.update
                  ? t("plugins.updateTo", { version: shown.version })
                  : t("plugins.installButton")}
            </Button>
            <Button disabled={busy} onClick={() => void choose(null)}>
              {t("plugins.cancel")}
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

  const use = plugin.kind === "check" ? t("plugins.asCheck") : t("plugins.asChannel");
  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-labelledby={`${id}-title`}>
      <h2 id={`${id}-title`} className={styles.sectionTitle}>
        {t("plugins.whereTitle", { name: plugin.title })}
      </h2>
      <p className={styles.dim}>{t("plugins.whereNote", { use })}</p>
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
          {busy ? t("plugins.saving") : t("plugins.save")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("plugins.cancel")}
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
    { key: "name", header: t("plugins.column.publisher"), cell: (p) => <strong>{p.name}</strong> },
    { key: "key", header: t("plugins.column.key"), cell: (p) => <span className={styles.mono}>{p.fingerprint}</span> },
    {
      key: "plugins",
      header: t("plugins.column.installed"),
      className: styles.wrap,
      cell: (p) => (p.plugins.length ? p.plugins.join(", ") : <span className={styles.dim}>{t("plugins.none")}</span>),
    },
    {
      key: "actions",
      header: "",
      cell: (p) => (
        <InlineConfirm
          layout="row"
          trigger={t("plugins.remove")}
          triggerName={t("plugins.removeName", { name: p.name })}
          title={t("plugins.stopTrusting", { name: p.name })}
          consequence={t("plugins.stopConsequence")}
          confirmLabel={t("plugins.remove")}
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
          {t("plugins.publishers")}
        </h2>
      </div>
      <p className={styles.sectionNote}>
        {rich("plugins.publishersNote", { command: <code>monitorflow-plugin keygen</code> })}
      </p>
      {error !== null && !data && <ErrorState what={t("plugins.publishersWhat")} error={describeError(error)} onRetry={refresh} />}
      {data && data.publishers.length > 0 && (
        <DataTable bare label={t("plugins.publishers")} columns={columns} rows={data.publishers} rowKey={(p) => p.id} />
      )}
      <form className={styles.panel} onSubmit={(e) => void add(e)} aria-label={t("plugins.addPublisher")}>
        <div className={styles.formGrid}>
          <label className={styles.field}>
            {t("plugins.name")}
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
            {t("plugins.publicKey")}
            <input
              className={fieldClass}
              required
              spellCheck={false}
              autoComplete="off"
              value={key}
              onChange={(e) => setKey(e.target.value)}
              placeholder={t("plugins.keyPlaceholder")}
            />
          </label>
        </div>
        <div className={styles.formFoot}>
          <Button type="submit" disabled={busy}>
            {busy ? t("plugins.adding") : t("plugins.trust")}
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
