"use client";

import { type FormEvent, useEffect, useMemo, useState } from "react";

import { type Combine, type Panel, type PanelServers, type Server, type Within, api, describeError, isAbortError } from "@/lib/api";
import {
  CHART_COMBINES,
  FIGURE_COMBINES,
  MAX_NAME,
  MAX_NAMED_SERVERS,
  PANEL_KINDS,
  combineLabel,
  defaultInstance,
  kindLabel,
  placesOf,
  suggestedTitle,
  widthLabel,
  withKind,
} from "@/lib/dashboards";
import { t } from "@/i18n";
import { useMultiCustomer } from "@/lib/me";
import { metricTitle, shortMetric } from "@/lib/metrics";
import { Button, fieldClass } from "@/components/ui/Button";
import { SegmentedControl } from "@/components/ui/SegmentedControl";

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
    if (panel.servers.kind === "list" && panel.servers.ids.length === 0) return setError(t("dashboards.form.chooseOne"));
    if (panel.servers.kind === "list" && panel.servers.ids.length > MAX_NAMED_SERVERS) {
      return setError(
        multiCustomer
          ? t("dashboards.form.atMostPlaces", { max: MAX_NAMED_SERVERS })
          : t("dashboards.form.atMostSite", { max: MAX_NAMED_SERVERS }),
      );
    }
    if (!panel.title.trim()) return setError(t("dashboards.form.titleRequired"));
    onDone({ ...panel, title: panel.title.trim() });
  }

  const metricOptions = [...new Set([...(metrics ?? []), ...(panel.metric ? [panel.metric] : [])])].sort((a, b) =>
    metricTitle(a).localeCompare(metricTitle(b)),
  );
  const combines = panel.kind === "chart" ? CHART_COMBINES : FIGURE_COMBINES;
  const serverChoices: [ServersChoice, string][] = [
    ["all", within.kind === "all" ? t("dashboards.form.every") : t("dashboards.form.everyHere")],
    ...(within.kind !== "site" && places.sites.length > 1 ? [["site", t("dashboards.form.oneSite")] as [ServersChoice, string]] : []),
    ...(multiCustomer && within.kind === "all" && places.customers.length > 1
      ? [["customer", t("dashboards.form.oneCustomer")] as [ServersChoice, string]]
      : []),
    ["list", t("dashboards.form.chosen")],
  ];

  return (
    <form className={styles.form} onSubmit={submit} aria-label={isNew ? t("dashboards.form.new") : t("dashboards.form.edit", { title: initial.title })}
    >
      <fieldset className={admin.fieldset}>
        <legend className={admin.legend}>{t("dashboards.form.shows")}</legend>
        <SegmentedControl
          label={t("dashboards.form.shows")}
          options={PANEL_KINDS.map((kind) => ({ value: kind, label: kindLabel(kind) }))}
          value={panel.kind}
          onChange={(kind) => change(withKind(panel, kind))}
        />
      </fieldset>

      <div className={admin.formGrid}>
        <label className={admin.field}>
          {t("dashboards.form.servers")}
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
            {t("dashboards.form.site")}
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
            {t("dashboards.form.customer")}
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
            {t("dashboards.form.chosen")}{" "}
            <span className={admin.fieldNote}>
              {t("dashboards.form.chosenCount", { count: chosen.length, max: MAX_NAMED_SERVERS })}
            </span>
          </legend>
          <input
            className={fieldClass}
            type="search"
            placeholder={t("dashboards.form.find")}
            aria-label={t("dashboards.form.find")}
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
              <span className={admin.fieldNote}>
                {t("dashboards.form.more", { count: matches.length - SHOWN_SERVERS })}
              </span>
            )}
            {matches.length === 0 && <span className={admin.fieldNote}>{t("dashboards.form.noMatch")}</span>}
          </div>
        </fieldset>
      )}

      {panel.kind !== "alerts" && (
        <div className={admin.formGrid}>
          <label className={admin.field}>
            {t("dashboards.form.metric")}
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
            {catalogError && <span className={admin.fieldNote}>{t("dashboards.form.metricsFailed", { error: catalogError })}</span>}
            {metrics && metrics.length === 0 && <span className={admin.fieldNote}>{t("dashboards.form.noMetrics")}</span>}
          </label>
          <label className={admin.field}>
            {t("dashboards.form.watches")}
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
              <option value="">{t("dashboards.form.all")}</option>
              {[...new Set([...(instances ?? []), ...(panel.instance ? [panel.instance] : [])])].map((i) => (
                <option key={i} value={i}>
                  {i}
                </option>
              ))}
            </select>
          </label>
          {(panel.kind === "chart" || panel.kind === "figure") && (
            <label className={admin.field}>
              {t("dashboards.form.combine")}
              <select
                className={fieldClass}
                value={panel.combine}
                onChange={(e) => change({ ...panel, combine: e.target.value as Combine })}
              >
                {combines.map((c) => (
                  <option key={c} value={c}>
                    {combineLabel(c)}
                  </option>
                ))}
              </select>
            </label>
          )}
          {panel.kind === "top" && (
            <label className={admin.field}>
              {t("dashboards.form.order")}
              <select
                className={fieldClass}
                value={panel.order}
                onChange={(e) => change({ ...panel, order: e.target.value as "highest" | "lowest" })}
              >
                <option value="highest">{t("dashboards.form.highestFirst")}</option>
                <option value="lowest">{t("dashboards.form.lowestFirst")}</option>
              </select>
            </label>
          )}
        </div>
      )}

      <div className={admin.formGrid}>
        {(panel.kind === "top" || panel.kind === "alerts") && (
          <label className={admin.field}>
            {t("dashboards.form.howMany")}
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
          {t("dashboards.form.width")}
          <select
            className={fieldClass}
            value={panel.width}
            onChange={(e) => change({ ...panel, width: Number(e.target.value) as Panel["width"] })}
          >
            {([1, 2, 3] as const).map((w) => (
              <option key={w} value={w}>
                {widthLabel(w)}
              </option>
            ))}
          </select>
        </label>
        <label className={admin.field}>
          {t("dashboards.form.title")}
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
          {isNew ? t("dashboards.form.add") : t("dashboards.form.done")}
        </Button>
        <Button onClick={onCancel}>{t("dashboards.form.cancel")}</Button>
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}
