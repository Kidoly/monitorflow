"use client";

import { type FormEvent, useEffect, useId, useMemo, useState } from "react";

import { type Combine, type Panel, type PanelKind, type PanelServers, type Server, type Within, api, describeError, isAbortError } from "@/lib/api";
import {
  CHART_COMBINES,
  COMBINE_LABELS,
  FIGURE_COMBINES,
  KIND_LABELS,
  MAX_NAME,
  MAX_NAMED_SERVERS,
  WIDTH_LABELS,
  defaultInstance,
  placesOf,
  suggestedTitle,
  withKind,
} from "@/lib/dashboards";
import { useMultiCustomer } from "@/lib/me";
import { metricTitle, shortMetric } from "@/lib/metrics";
import { Button, fieldClass } from "@/components/ui/Button";

import admin from "./AdminPages.module.css";
import styles from "./Dashboards.module.css";

/** Servers listed at once in "Chosen servers"; the search narrows the rest. */
const SHOWN_SERVERS = 100;

function useCatalog(within: Within, metric: string | undefined) {
  const [metrics, setMetrics] = useState<string[] | null>(null);
  const [instances, setInstances] = useState<{ metric: string; list: string[] } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const query = within.kind === "site" ? { siteId: within.id } : within.kind === "customer" ? { customerId: within.id } : {};
  const key = JSON.stringify(query);
  useEffect(() => {
    const controller = new AbortController();
    api.dashboardCatalog(JSON.parse(key) as typeof query, controller.signal).then(
      (c) => setMetrics(c.metrics),
      (e: unknown) => {
        if (!isAbortError(e)) setError(describeError(e));
      },
    );
    return () => controller.abort();
  }, [key]);
  useEffect(() => {
    if (!metric) return;
    const controller = new AbortController();
    api.dashboardCatalog({ ...(JSON.parse(key) as typeof query), metric }, controller.signal).then(
      (c) => setInstances({ metric, list: c.instances }),
      (e: unknown) => {
        if (!isAbortError(e)) setInstances({ metric, list: [] });
      },
    );
    return () => controller.abort();
  }, [key, metric]);
  return { metrics, instances: instances && instances.metric === metric ? instances.list : null, error };
}

type ServersChoice = PanelServers["kind"];

/**
 * The form of one panel: what it shows, of which servers, which metric and
 * how servers combine, all picked from lists. The title follows the
 * choices until it is typed.
 */
export function PanelForm({
  initial,
  isNew,
  servers,
  within,
  onDone,
  onCancel,
}: {
  initial: Panel;
  isNew: boolean;
  /** The servers the dashboard may name. */
  servers: Server[];
  within: Within;
  onDone: (panel: Panel) => void;
  onCancel: () => void;
}) {
  const multiCustomer = useMultiCustomer();
  const id = useId();
  const [draft, setDraft] = useState(initial);
  const [titleTyped, setTitleTyped] = useState(!isNew && initial.title !== suggestedTitle(initial));
  const [instanceChosen, setInstanceChosen] = useState(!isNew);
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const { metrics, instances, error: catalogError } = useCatalog(within, draft.metric);
  const places = useMemo(() => placesOf(servers), [servers]);

  // A new panel watches the total when the metric has one, until told otherwise.
  const auto = !instanceChosen && draft.instance === undefined && instances ? defaultInstance(instances) : undefined;
  const watched = auto ? { ...draft, instance: auto } : draft;
  const panel = auto && !titleTyped ? { ...watched, title: suggestedTitle(watched) } : watched;

  function change(next: Panel) {
    setDraft(titleTyped ? next : { ...next, title: suggestedTitle(next) });
    setError(null);
  }

  const chosen = panel.servers.kind === "list" ? panel.servers.ids : [];
  const matches = servers.filter((s) => s.hostname.toLowerCase().includes(search.trim().toLowerCase()));

  function setServersKind(kind: ServersChoice) {
    const first = kind === "site" ? places.sites[0]?.id : kind === "customer" ? places.customers[0]?.id : undefined;
    const next: PanelServers =
      kind === "all" ? { kind } : kind === "list" ? { kind, ids: chosen } : { kind, id: first ?? "" };
    change({ ...panel, servers: next });
  }

  function toggleServer(serverId: string, on: boolean) {
    const ids = on ? [...chosen, serverId] : chosen.filter((x) => x !== serverId);
    change({ ...panel, servers: { kind: "list", ids } });
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (panel.servers.kind === "list" && panel.servers.ids.length === 0) return setError("Choose at least one server.");
    if (panel.servers.kind === "list" && panel.servers.ids.length > MAX_NAMED_SERVERS) {
      return setError(`Choose at most ${MAX_NAMED_SERVERS} servers, or ${multiCustomer ? "a site or customer" : "a site"}.`);
    }
    if (!panel.title.trim()) return setError("Give the panel a title.");
    onDone({ ...panel, title: panel.title.trim() });
  }

  const metricOptions = [...new Set([...(metrics ?? []), ...(panel.metric ? [panel.metric] : [])])].sort((a, b) =>
    metricTitle(a).localeCompare(metricTitle(b)),
  );
  const combines = panel.kind === "chart" ? CHART_COMBINES : FIGURE_COMBINES;
  const serverChoices: [ServersChoice, string][] = [
    ["all", within.kind === "all" ? "Every server" : "Every server of this dashboard"],
    ...(within.kind !== "site" && places.sites.length > 1 ? [["site", "One site"] as [ServersChoice, string]] : []),
    ...(multiCustomer && within.kind === "all" && places.customers.length > 1 ? [["customer", "One customer"] as [ServersChoice, string]] : []),
    ["list", "Chosen servers"],
  ];

  return (
    <form className={styles.form} onSubmit={submit} aria-label={isNew ? "New panel" : `Edit ${initial.title}`}>
      <fieldset className={admin.fieldset}>
        <legend className={admin.legend}>Shows</legend>
        <div className={styles.choices}>
          {(Object.keys(KIND_LABELS) as PanelKind[]).map((kind) => (
            <label key={kind}>
              <input type="radio" name={`${id}-kind`} checked={panel.kind === kind} onChange={() => change(withKind(panel, kind))} />
              {KIND_LABELS[kind]}
            </label>
          ))}
        </div>
      </fieldset>

      <div className={admin.formGrid}>
        <label className={admin.field}>
          Servers
          <select className={fieldClass} value={panel.servers.kind} onChange={(e) => setServersKind(e.target.value as ServersChoice)}>
            {serverChoices.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
        {panel.servers.kind === "site" && (
          <label className={admin.field}>
            Site
            <select
              className={fieldClass}
              value={panel.servers.id}
              onChange={(e) => change({ ...panel, servers: { kind: "site", id: e.target.value } })}
            >
              {places.sites.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </select>
          </label>
        )}
        {panel.servers.kind === "customer" && (
          <label className={admin.field}>
            Customer
            <select
              className={fieldClass}
              value={panel.servers.id}
              onChange={(e) => change({ ...panel, servers: { kind: "customer", id: e.target.value } })}
            >
              {places.customers.map((c) => (
                <option key={c.id} value={c.id}>
                  {c.name}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>

      {panel.servers.kind === "list" && (
        <fieldset className={admin.fieldset}>
          <legend className={admin.legend}>
            Chosen servers <span className={admin.fieldNote}>{chosen.length} of at most {MAX_NAMED_SERVERS}</span>
          </legend>
          <input
            className={fieldClass}
            type="search"
            placeholder="Find a server"
            aria-label="Find a server"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
          />
          <div className={styles.serverList}>
            {matches.slice(0, SHOWN_SERVERS).map((s) => (
              <label key={s.id}>
                <input type="checkbox" checked={chosen.includes(s.id)} onChange={(e) => toggleServer(s.id, e.target.checked)} />
                {s.hostname} <span className={admin.fieldNote}>{s.site_name}</span>
              </label>
            ))}
            {matches.length > SHOWN_SERVERS && (
              <span className={admin.fieldNote}>{matches.length - SHOWN_SERVERS} more: type part of a name to find them.</span>
            )}
            {matches.length === 0 && <span className={admin.fieldNote}>No server matches.</span>}
          </div>
        </fieldset>
      )}

      {panel.kind !== "alerts" && (
        <div className={admin.formGrid}>
          <label className={admin.field}>
            Metric
            <select
              className={fieldClass}
              value={panel.metric}
              onChange={(e) => {
                const { instance: _dropped, ...rest } = panel;
                void _dropped;
                setInstanceChosen(false);
                change({ ...rest, metric: e.target.value });
              }}
            >
              {metricOptions.map((m) => (
                <option key={m} value={m}>
                  {metricTitle(m) === shortMetric(m) ? m : `${metricTitle(m)} (${shortMetric(m)})`}
                </option>
              ))}
            </select>
            {catalogError && <span className={admin.fieldNote}>Could not list the metrics: {catalogError}</span>}
            {metrics && metrics.length === 0 && <span className={admin.fieldNote}>These servers reported no metric in the last day.</span>}
          </label>
          <label className={admin.field}>
            Watches
            <select
              className={fieldClass}
              value={panel.instance ?? ""}
              onChange={(e) => {
                const { instance: _old, ...rest } = panel;
                void _old;
                setInstanceChosen(true);
                change(e.target.value ? { ...rest, instance: e.target.value } : rest);
              }}
            >
              <option value="">All</option>
              {[...new Set([...(instances ?? []), ...(panel.instance ? [panel.instance] : [])])].map((i) => (
                <option key={i} value={i}>
                  {i}
                </option>
              ))}
            </select>
          </label>
          {(panel.kind === "chart" || panel.kind === "figure") && (
            <label className={admin.field}>
              Servers combine as
              <select
                className={fieldClass}
                value={panel.combine}
                onChange={(e) => change({ ...panel, combine: e.target.value as Combine })}
              >
                {combines.map((c) => (
                  <option key={c} value={c}>
                    {COMBINE_LABELS[c]}
                  </option>
                ))}
              </select>
            </label>
          )}
          {panel.kind === "top" && (
            <label className={admin.field}>
              Order
              <select
                className={fieldClass}
                value={panel.order}
                onChange={(e) => change({ ...panel, order: e.target.value as "highest" | "lowest" })}
              >
                <option value="highest">Highest first</option>
                <option value="lowest">Lowest first</option>
              </select>
            </label>
          )}
        </div>
      )}

      <div className={admin.formGrid}>
        {(panel.kind === "top" || panel.kind === "alerts") && (
          <label className={admin.field}>
            How many
            <input
              className={fieldClass}
              type="number"
              min={1}
              max={20}
              value={panel.count}
              onChange={(e) => change({ ...panel, count: Math.min(20, Math.max(1, Number(e.target.value) || 1)) })}
            />
          </label>
        )}
        <label className={admin.field}>
          Width
          <select
            className={fieldClass}
            value={panel.width}
            onChange={(e) => change({ ...panel, width: Number(e.target.value) as Panel["width"] })}
          >
            {([1, 2, 3] as const).map((w) => (
              <option key={w} value={w}>
                {WIDTH_LABELS[w]}
              </option>
            ))}
          </select>
        </label>
        <label className={admin.field}>
          Title
          <input
            className={fieldClass}
            required
            maxLength={MAX_NAME}
            value={panel.title}
            onChange={(e) => {
              setTitleTyped(true);
              setDraft({ ...panel, title: e.target.value });
            }}
          />
        </label>
      </div>

      <div className={admin.formFoot}>
        <Button type="submit" variant="k">
          {isNew ? "Add panel" : "Done"}
        </Button>
        <Button onClick={onCancel}>Cancel</Button>
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}
