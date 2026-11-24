"use client";

import { type FormEvent, useEffect, useId, useMemo, useState } from "react";

import {
  type Combine,
  type DashboardData,
  type MapSummary,
  type Panel,
  type PanelServers,
  type PanelUnit,
  type Server,
  type TimeRangeLabel,
  type Within,
  api,
  describeError,
  isAbortError,
} from "@/lib/api";
import {
  CHART_COMBINES,
  COMMON_METRICS,
  FIGURE_COMBINES,
  KINDS_FOR,
  MAX_NAME,
  MAX_NAMED_SERVERS,
  MAX_TABLE_COLUMNS,
  MAX_TABLE_ROWS,
  MAX_TEXT,
  type PanelSource,
  baseKind,
  combineLabel,
  defaultInstance,
  placesOf,
  sourceOf,
  suggestedTitle,
  widthLabel,
  withKind,
  withSource,
} from "@/lib/dashboards";
import { t } from "@/i18n";
import { useMultiCustomer } from "@/lib/me";
import { metricTitle, shortMetric } from "@/lib/metrics";
import { useScope } from "@/lib/scope";
import { Button, fieldClass } from "@/components/ui/Button";
import { Switch } from "@/components/ui/Switch";

import admin from "./AdminPages.module.css";
import { DashboardPanel } from "./DashboardPanel";
import styles from "./Dashboards.module.css";
import { KindTiles } from "./KindTiles";

/** Servers listed at once in "Chosen servers"; the search narrows the rest. */
const SHOWN_SERVERS = 100;
/** How long the preview waits for the choices to settle. */
const PREVIEW_DELAY_MS = 400;

const UNITS: PanelUnit[] = ["ratio", "bytes", "bytes_per_second", "bits_per_second", "per_second", "seconds", "celsius", "number"];

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

/** The panel's data as it would show now, a moment after the last change. */
function usePreview(panel: Panel, range: TimeRangeLabel, within: Within, ready: boolean) {
  const [result, setResult] = useState<{ key: string; data?: DashboardData; error?: string } | null>(null);
  // Title is not data: a blank one while typing still previews.
  const key = JSON.stringify({ range, within, panel: { ...panel, title: panel.title.trim() || suggestedTitle(panel) } });
  useEffect(() => {
    if (!ready || panel.kind === "text" || (panel.kind === "map" && !panel.map_id)) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      const body = JSON.parse(key) as { range: TimeRangeLabel; within: Within; panel: Panel };
      api.dashboardData({ range: body.range, within: body.within, panels: [body.panel] }, controller.signal).then(
        (data) => setResult({ key, data }),
        (e: unknown) => {
          if (!isAbortError(e)) setResult({ key, error: describeError(e) });
        },
      );
    }, PREVIEW_DELAY_MS);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [key, ready, panel.kind, panel.map_id]);
  return result;
}

/** The network maps the user sees, read once a map panel is chosen (ADR-0185). */
function useMaps(wanted: boolean): MapSummary[] | null {
  const [maps, setMaps] = useState<MapSummary[] | null>(null);
  useEffect(() => {
    if (!wanted || maps) return;
    const controller = new AbortController();
    api.maps(controller.signal).then(
      (answer) => setMaps(answer.maps),
      () => {
        if (!controller.signal.aborted) setMaps([]);
      },
    );
    return () => controller.abort();
  }, [wanted, maps]);
  return maps;
}

type ServersChoice = PanelServers["kind"];

/** A number field's value, or undefined when empty or not a number. */
const numberOf = (text: string): number | undefined => {
  const value = Number(text.replace(",", "."));
  return text.trim() === "" || !Number.isFinite(value) ? undefined : value;
};

/** The panel without `key`, so an absent field stays absent. */
function without<K extends keyof Panel>(panel: Panel, key: K): Panel {
  const next = { ...panel };
  delete next[key];
  return next;
}

/**
 * The form of one panel (ADR-0178): what to show and of which servers,
 * how to draw it as tiles, and a preview with live data. Everything else
 * is under "Advanced", closed, with defaults that make it unnecessary.
 * The title follows the choices until it is typed.
 */
export function PanelForm({
  initial,
  isNew,
  servers,
  within,
  range = "6h",
  onDone,
  onCancel,
}: {
  initial: Panel;
  isNew: boolean;
  /** The servers the dashboard may name. */
  servers: Server[];
  within: Within;
  /** The dashboard's range, for the preview. */
  range?: TimeRangeLabel;
  onDone: (panel: Panel) => void;
  onCancel: () => void;
}) {
  const multiCustomer = useMultiCustomer();
  const scope = useScope();
  const id = useId();
  const [draft, setDraft] = useState(initial);
  const [titleTyped, setTitleTyped] = useState(!isNew && initial.title !== suggestedTitle(initial));
  const [instanceChosen, setInstanceChosen] = useState(!isNew);
  const [search, setSearch] = useState("");
  const [error, setError] = useState<string | null>(null);
  const { metrics, instances, error: catalogError } = useCatalog(within, draft.metric ?? draft.columns?.[0]?.metric);
  const places = useMemo(() => placesOf(servers), [servers]);

  // A new panel watches the total when the metric has one, until told otherwise.
  const auto = !instanceChosen && draft.metric && draft.instance === undefined && instances ? defaultInstance(instances) : undefined;
  const watched = auto ? { ...draft, instance: auto } : draft;
  const panel = auto && !titleTyped ? { ...watched, title: suggestedTitle(watched) } : watched;
  const source = sourceOf(panel);
  const maps = useMaps(source === "map");

  function change(next: Panel) {
    setDraft(titleTyped ? next : { ...next, title: suggestedTitle(next) });
    setError(null);
  }

  const chosen = panel.servers.kind === "list" ? panel.servers.ids : [];
  const matches = servers.filter((s) => s.hostname.toLowerCase().includes(search.trim().toLowerCase()));
  const previewReady = panel.servers.kind !== "list" || (chosen.length > 0 && chosen.length <= MAX_NAMED_SERVERS);
  const preview = usePreview(panel, range, within, previewReady);
  const previewKey = JSON.stringify({ range, within, panel: { ...panel, title: panel.title.trim() || suggestedTitle(panel) } });
  const fresh = preview?.key === previewKey ? preview : undefined;

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

  /** "states", "alerts", "text" or "metric:<name>", the value of the Show list. */
  const showValue = source === "metric" ? `metric:${panel.metric ?? panel.columns?.[0]?.metric ?? ""}` : source;

  function show(value: string) {
    if (value.startsWith("metric:")) {
      const metric = value.slice("metric:".length);
      setInstanceChosen(false);
      if (panel.kind === "table") {
        // The table's first column, which orders it.
        const columns = [{ metric }, ...(panel.columns ?? []).slice(1)];
        return change({ ...panel, columns });
      }
      return change(withSource(panel, "metric", metric));
    }
    change(withSource(panel, value as PanelSource));
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
    if (panel.kind === "text" && !panel.text?.trim()) return setError(t("dashboards.form.textRequired"));
    if (panel.kind === "map" && !panel.map_id) return setError(t("dashboards.form.mapRequired"));
    if (!panel.title.trim()) return setError(t("dashboards.form.titleRequired"));
    onDone({ ...panel, title: panel.title.trim() });
  }

  const byTitle = (a: string, b: string) => metricTitle(a).localeCompare(metricTitle(b));
  const known = [...new Set([...(metrics ?? []), ...(panel.metric ? [panel.metric] : [])])];
  const common = COMMON_METRICS.filter((m) => known.includes(m));
  const others = known.filter((m) => !common.includes(m)).sort(byTitle);
  const metricName = (m: string) => (metricTitle(m) === shortMetric(m) ? m : metricTitle(m));
  const serverChoices: [ServersChoice, string][] = [
    ["all", within.kind === "all" ? t("dashboards.form.every") : t("dashboards.form.everyHere")],
    ...(within.kind !== "site" && places.sites.length > 1 ? [["site", t("dashboards.form.oneSite")] as [ServersChoice, string]] : []),
    ...(multiCustomer && within.kind === "all" && places.customers.length > 1
      ? [["customer", t("dashboards.form.oneCustomer")] as [ServersChoice, string]]
      : []),
    ["list", t("dashboards.form.chosen")],
  ];
  const metricShown = panel.metric !== undefined && panel.kind !== "table";
  const hasValues = metricShown;
  const combines = baseKind(panel.kind) === "chart" ? CHART_COMBINES : FIGURE_COMBINES;
  const maxCount = panel.kind === "table" ? MAX_TABLE_ROWS : 20;

  return (
    <form className={styles.form} onSubmit={submit} aria-label={isNew ? t("dashboards.form.new") : t("dashboards.form.edit", { title: initial.title })}>
      <div className={styles.editor}>
        <div className={styles.editorChoices}>
          <div className={admin.formGrid}>
            <label className={admin.field}>
              {t("dashboards.form.show")}
              <select className={fieldClass} value={showValue} onChange={(e) => show(e.target.value)}>
                <optgroup label={t("dashboards.form.health")}>
                  <option value="states">{t("dashboards.form.showStates")}</option>
                  <option value="alerts">{t("dashboards.form.showAlerts")}</option>
                  <option value="text">{t("dashboards.form.showText")}</option>
                  <option value="map">{t("dashboards.form.showMap")}</option>
                </optgroup>
                {common.length > 0 && (
                  <optgroup label={t("dashboards.form.common")}>
                    {common.map((m) => (
                      <option key={m} value={`metric:${m}`}>
                        {metricName(m)}
                      </option>
                    ))}
                  </optgroup>
                )}
                <optgroup label={t("dashboards.form.allMetrics")}>
                  {others.map((m) => (
                    <option key={m} value={`metric:${m}`}>
                      {metricTitle(m) === shortMetric(m) ? m : `${metricTitle(m)} (${shortMetric(m)})`}
                    </option>
                  ))}
                </optgroup>
              </select>
              {catalogError && <span className={admin.fieldNote}>{t("dashboards.form.metricsFailed", { error: catalogError })}</span>}
              {metrics && metrics.length === 0 && <span className={admin.fieldNote}>{t("dashboards.form.noMetrics")}</span>}
            </label>
            {source === "map" && (
              <label className={admin.field}>
                {t("dashboards.form.map")}
                <select className={fieldClass} value={panel.map_id ?? ""} onChange={(e) => change({ ...panel, map_id: e.target.value || undefined })}>
                  <option value="">{t("dashboards.form.chooseMap")}</option>
                  {(maps ?? []).map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.name} ({m.place.name})
                    </option>
                  ))}
                </select>
                {maps && maps.length === 0 && <span className={admin.fieldNote}>{t("dashboards.form.noMaps")}</span>}
              </label>
            )}
            {source !== "text" && source !== "map" && (
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
            )}
            {panel.servers.kind === "site" && (
              <label className={admin.field}>
                {t("dashboards.form.site")}
                <select className={fieldClass} value={panel.servers.id} onChange={(e) => change({ ...panel, servers: { kind: "site", id: e.target.value } })}>
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
                <span className={admin.fieldNote}>{t("dashboards.form.chosenCount", { count: chosen.length, max: MAX_NAMED_SERVERS })}</span>
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
                  <span className={admin.fieldNote}>{t("dashboards.form.more", { count: matches.length - SHOWN_SERVERS })}</span>
                )}
                {matches.length === 0 && <span className={admin.fieldNote}>{t("dashboards.form.noMatch")}</span>}
              </div>
            </fieldset>
          )}

          {panel.kind === "text" && (
            <label className={admin.field}>
              {t("dashboards.form.text")}
              <textarea
                className={`${fieldClass} ${styles.textarea}`}
                rows={6}
                maxLength={MAX_TEXT}
                value={panel.text ?? ""}
                onChange={(e) => change({ ...panel, text: e.target.value })}
              />
              <span className={admin.fieldNote}>{t("dashboards.form.textNote")}</span>
            </label>
          )}

          {KINDS_FOR[source].length > 1 && (
            <fieldset className={admin.fieldset}>
              <legend className={admin.legend}>{t("dashboards.form.drawAs")}</legend>
              <KindTiles label={t("dashboards.form.drawAs")} kinds={KINDS_FOR[source]} value={panel.kind} onChange={(kind) => change(withKind(panel, kind))} />
            </fieldset>
          )}
        </div>

        <section className={styles.preview} aria-label={t("dashboards.form.preview")}>
          <h3 className={admin.legend}>{t("dashboards.form.preview")}</h3>
          {fresh?.error ? (
            <p className={styles.error} role="alert">
              {fresh.error}
            </p>
          ) : (
            <DashboardPanel panel={panel} data={fresh?.data?.panels[0]} window={fresh?.data} scope={scope} />
          )}
        </section>
      </div>

      <details className={styles.advanced}>
        <summary>{t("dashboards.form.advanced")}</summary>
        <div className={admin.formGrid}>
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
          <label className={admin.field}>
            {t("dashboards.form.width")}
            <select className={fieldClass} value={panel.width} onChange={(e) => change({ ...panel, width: Number(e.target.value) as Panel["width"] })}>
              {([1, 2, 3] as const).map((w) => (
                <option key={w} value={w}>
                  {widthLabel(w)}
                </option>
              ))}
            </select>
          </label>
          {metricShown && (
            <label className={admin.field}>
              {t("dashboards.form.watches")}
              <select
                className={fieldClass}
                value={panel.instance ?? ""}
                onChange={(e) => {
                  setInstanceChosen(true);
                  const rest = without(panel, "instance");
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
          )}
          {(baseKind(panel.kind) === "chart" || baseKind(panel.kind) === "figure") && panel.kind !== "heatmap" && (
            <label className={admin.field}>
              {t("dashboards.form.combine")}
              <select className={fieldClass} value={panel.combine} onChange={(e) => change({ ...panel, combine: e.target.value as Combine })}>
                {combines.map((c) => (
                  <option key={c} value={c}>
                    {combineLabel(c)}
                  </option>
                ))}
              </select>
            </label>
          )}
          {(baseKind(panel.kind) === "top" || panel.kind === "table") && (
            <label className={admin.field}>
              {t("dashboards.form.order")}
              <select className={fieldClass} value={panel.order} onChange={(e) => change({ ...panel, order: e.target.value as "highest" | "lowest" })}>
                <option value="highest">{t("dashboards.form.highestFirst")}</option>
                <option value="lowest">{t("dashboards.form.lowestFirst")}</option>
              </select>
            </label>
          )}
          {panel.count !== undefined && (
            <label className={admin.field}>
              {t("dashboards.form.howMany")}
              <input
                className={fieldClass}
                type="number"
                min={1}
                max={maxCount}
                value={panel.count}
                onChange={(e) => change({ ...panel, count: Math.min(maxCount, Math.max(1, Number(e.target.value) || 1)) })}
              />
            </label>
          )}
        </div>

        {panel.kind === "table" && (
          <fieldset className={admin.fieldset}>
            <legend className={admin.legend}>{t("dashboards.form.columns")}</legend>
            {(panel.columns ?? []).map((column, i) => (
              <div key={i} className={styles.columnRow}>
                <select
                  className={fieldClass}
                  aria-label={t("dashboards.form.column", { n: i + 1 })}
                  value={column.metric}
                  onChange={(e) => change({ ...panel, columns: (panel.columns ?? []).map((c, k) => (k === i ? { metric: e.target.value } : c)) })}
                >
                  {[...new Set([...known, column.metric])].sort(byTitle).map((m) => (
                    <option key={m} value={m}>
                      {metricName(m)}
                    </option>
                  ))}
                </select>
                {i > 0 && (
                  <Button size="small" onClick={() => change({ ...panel, columns: (panel.columns ?? []).filter((_, k) => k !== i) })}>
                    {t("dashboards.form.removeColumn")}
                  </Button>
                )}
              </div>
            ))}
            {(panel.columns?.length ?? 0) < MAX_TABLE_COLUMNS && (
              <Button
                size="small"
                onClick={() => {
                  const used = new Set(panel.columns?.map((c) => c.metric));
                  const next = [...COMMON_METRICS, ...known].find((m) => !used.has(m) && known.includes(m)) ?? known[0];
                  if (next) change({ ...panel, columns: [...(panel.columns ?? []), { metric: next }] });
                }}
              >
                {t("dashboards.form.addColumn")}
              </Button>
            )}
          </fieldset>
        )}

        {hasValues && (
          <div className={admin.formGrid}>
            <label className={admin.field}>
              {t("dashboards.form.unit")}
              <select
                className={fieldClass}
                value={panel.unit ?? ""}
                onChange={(e) => change(e.target.value ? { ...panel, unit: e.target.value as PanelUnit } : without(panel, "unit"))}
              >
                <option value="">{t("dashboards.form.unitAuto")}</option>
                {UNITS.map((u) => (
                  <option key={u} value={u}>
                    {t(`dashboards.unit.${u}`)}
                  </option>
                ))}
              </select>
            </label>
            {(["min", "max"] as const).map((end) => (
              <label key={end} className={admin.field}>
                {t(`dashboards.form.${end}`)}
                <input
                  className={fieldClass}
                  inputMode="decimal"
                  placeholder={t("dashboards.form.auto")}
                  defaultValue={panel[end] ?? ""}
                  onChange={(e) => {
                    const value = numberOf(e.target.value);
                    change(value === undefined ? without(panel, end) : { ...panel, [end]: value });
                  }}
                />
              </label>
            ))}
            {(["warning", "critical"] as const).map((level) => (
              <label key={level} className={admin.field}>
                {t(`dashboards.form.${level}`)}
                <input
                  className={fieldClass}
                  inputMode="decimal"
                  placeholder={t("dashboards.form.none")}
                  defaultValue={panel.thresholds?.[level] ?? ""}
                  onChange={(e) => {
                    const value = numberOf(e.target.value);
                    const thresholds = { ...panel.thresholds, [level]: value };
                    if (value === undefined) delete thresholds[level];
                    change(Object.keys(thresholds).length ? { ...panel, thresholds } : without(panel, "thresholds"));
                  }}
                />
              </label>
            ))}
          </div>
        )}
        {hasValues && <p className={admin.fieldNote}>{t("dashboards.form.thresholdsNote")}</p>}

        {hasValues && (
          <div className={styles.optionsRow}>
            <fieldset className={admin.fieldset}>
              <legend className={admin.legend}>{t("dashboards.form.color")}</legend>
              <div className={styles.swatches}>
                {[1, 2, 3, 4, 5, 6, 7, 8].map((c) => (
                  <label key={c} className={styles.swatchChoice} data-checked={(panel.color ?? 1) === c || undefined} style={{ background: `var(--series-${c})` }}>
                    <input
                      type="radio"
                      name={`${id}-color`}
                      checked={(panel.color ?? 1) === c}
                      aria-label={t("dashboards.form.colorN", { n: c })}
                      onChange={() => change(c === 1 ? without(panel, "color") : { ...panel, color: c })}
                    />
                  </label>
                ))}
              </div>
            </fieldset>
            {["chart", "area", "columns"].includes(panel.kind) && (
              <Switch
                checked={panel.legend ?? true}
                onChange={(checked) => change(checked ? without(panel, "legend") : { ...panel, legend: false })}
                label={t("dashboards.form.legend")}
              />
            )}
          </div>
        )}
      </details>

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
