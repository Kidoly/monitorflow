"use client";

// The richer forms of the alert editor (ADR-0153): what a rule judges, its
// fields, conditions joined with "and", a live preview of what it would
// have done, and the rules MonitorFlow suggests.

import { useEffect, useId, useState } from "react";

import { t } from "@/i18n";
import {
  type Comparison,
  type ConfigOwner,
  type RecommendedThreshold,
  type ServerPreview,
  type SitePreview,
  type ThresholdRequest,
  api,
  describeError,
} from "@/lib/api";
import type { Shape } from "@/lib/alerts";
import { formatHourMinute, formatWhen } from "@/lib/clock";
import { metricTitle } from "@/lib/metrics";
import {
  COMPARE_FUNCTIONS,
  type ConditionForm,
  OFFSETS,
  type RuleErrors,
  type RuleForm,
  WINDOW_FUNCTIONS,
  judgedValue,
  kindLabel,
  kindsFor,
  levelUnit,
} from "@/lib/rules";
import { SEVERITY_LABELS } from "@/lib/thresholds";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { TimeChart } from "@/components/ui/TimeChart";

import admin from "./AdminPages.module.css";
import styles from "./MonitoringSettings.module.css";

/** Debounce of the preview while fields change, in milliseconds. */
const PREVIEW_DELAY_MS = 600;
/** Periods the preview offers, in days. */
const PERIODS = [1, 3, 7] as const;
/** Conditions joined with "and" to one rule. */
const MAX_CONDITIONS = 2;

function NumberField({
  label,
  unit,
  value,
  onChange,
  error,
  placeholder,
}: {
  label: string;
  unit?: string;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  placeholder?: string;
}) {
  const id = useId();
  const hintId = `${id}-hint`;
  return (
    <div className={admin.field}>
      <label htmlFor={id}>{label}</label>
      <span className={styles.unit}>
        <input
          id={id}
          className={cx(fieldClass, styles.number, error && admin.fieldError)}
          inputMode="decimal"
          value={value}
          onChange={(e) => onChange(e.target.value)}
          placeholder={placeholder}
          aria-invalid={error ? true : undefined}
          aria-describedby={error ? hintId : undefined}
        />
        {unit && <span>{unit}</span>}
      </span>
      {error && (
        <span id={hintId} className={styles.error} role="alert">
          {error}
        </span>
      )}
    </div>
  );
}

function Choice<T extends string | number>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  return (
    <label className={admin.field}>
      {label}
      <select
        className={fieldClass}
        value={String(value)}
        onChange={(e) => onChange(options.find((o) => String(o.value) === e.target.value)!.value)}
      >
        {options.map((o) => (
          <option key={String(o.value)} value={String(o.value)}>
            {o.label}
          </option>
        ))}
      </select>
    </label>
  );
}

/** "Alert on": the kinds a measure offers, in plain words. */
export function KindSelect({ shape, value, onChange }: { shape: Shape; value: RuleForm["kind"]; onChange: (kind: RuleForm["kind"]) => void }) {
  return (
    <Choice
      label={t("rules.editor.alertOn")}
      value={value}
      options={kindsFor(shape).map((kind) => ({ value: kind, label: kindLabel(kind) }))}
      onChange={onChange}
    />
  );
}

/**
 * The fields of a kind other than the simple threshold, which keeps the
 * editor's own: a window, a comparison, a forecast, a silence.
 */
export function KindFields({
  shape,
  form,
  set,
  errors,
}: {
  shape: Shape;
  form: RuleForm;
  set: <K extends keyof RuleForm>(key: K, value: RuleForm[K]) => void;
  errors: RuleErrors;
}) {
  const unit = levelUnit(shape, form);
  const levels = (warnLabel: string, critLabel: string) => (
    <>
      <NumberField label={warnLabel} unit={unit} value={form.warning} onChange={(v) => set("warning", v)} error={errors.warning} placeholder={t("monitoring.noWarning")} />
      <NumberField label={critLabel} unit={unit} value={form.critical} onChange={(v) => set("critical", v)} error={errors.critical} placeholder={t("monitoring.noCritical")} />
    </>
  );
  switch (form.kind) {
    case "window":
      return (
        <>
          <Choice
            label={t("rules.editor.function")}
            value={form.function}
            options={WINDOW_FUNCTIONS.map((f) => ({ value: f, label: t(`rules.function.${f}`) }))}
            onChange={(v) => set("function", v)}
          />
          <NumberField label={t("rules.editor.over")} unit={t("format.unit.minute")} value={form.window} onChange={(v) => set("window", v)} error={errors.window} />
        </>
      );
    case "compare":
      return (
        <>
          <Choice
            label={t("rules.editor.comparedWith")}
            value={form.offset}
            options={OFFSETS.map((o) => ({ value: o, label: t(`rules.offset.${o}`) }))}
            onChange={(v) => set("offset", v)}
          />
          <Choice
            label={t("rules.editor.function")}
            value={form.function}
            options={COMPARE_FUNCTIONS.map((f) => ({ value: f, label: t(`rules.function.${f}`) }))}
            onChange={(v) => set("function", v)}
          />
          <NumberField label={t("rules.editor.averagedOver")} unit={t("format.unit.minute")} value={form.window} onChange={(v) => set("window", v)} error={errors.window} />
          {levels(t("monitoring.warningAt"), t("monitoring.criticalAt"))}
        </>
      );
    case "forecast":
      return (
        <>
          <NumberField
            label={t("rules.editor.target")}
            unit={shape.type === "percent" ? "%" : shape.type === "number" ? shape.unit : undefined}
            value={form.target}
            onChange={(v) => set("target", v)}
            error={errors.target}
          />
          {levels(t("rules.editor.warnBefore"), t("rules.editor.criticalBefore"))}
          <NumberField label={t("rules.editor.learnOver")} unit={t("format.unit.hour")} value={form.learn} onChange={(v) => set("learn", v)} error={errors.learn} />
        </>
      );
    case "nodata":
      return levels(t("rules.editor.warnAfter"), t("rules.editor.criticalAfter"));
    default:
      return null;
  }
}

/** "And also when…": up to two more conditions on the same server. */
export function ConditionFields({
  metrics,
  conditions,
  onChange,
  error,
}: {
  /** Full metric names the server reports. */
  metrics: string[];
  conditions: ConditionForm[];
  onChange: (conditions: ConditionForm[]) => void;
  error?: string;
}) {
  const update = (i: number, change: Partial<ConditionForm>) =>
    onChange(conditions.map((c, j) => (j === i ? { ...c, ...change } : c)));
  return (
    <fieldset className={styles.scope}>
      {conditions.length > 0 && <legend>{t("rules.editor.conditionsLabel")}</legend>}
      {conditions.map((c, i) => (
        <div key={i} className={styles.fields}>
          <Choice
            label={t("rules.editor.conditionMetric")}
            value={c.metric}
            options={metrics.map((m) => ({ value: m, label: metricTitle(m) }))}
            onChange={(metric) => update(i, { metric })}
          />
          <Choice<Comparison>
            label={t("rules.editor.conditionComparison")}
            value={c.comparison}
            options={([">", "<"] as const).map((v) => ({ value: v, label: t(`checks.word.${v}`) }))}
            onChange={(comparison) => update(i, { comparison })}
          />
          <NumberField
            label={t("rules.editor.conditionValue")}
            unit={c.metric.endsWith("_ratio") ? "%" : undefined}
            value={c.value}
            onChange={(value) => update(i, { value })}
            error={i === conditions.length - 1 ? error : undefined}
          />
          <Button size="small" onClick={() => onChange(conditions.filter((_, j) => j !== i))}>
            {t("rules.editor.removeCondition")}
          </Button>
        </div>
      ))}
      {conditions.length < MAX_CONDITIONS && metrics.length > 0 && (
        <Button size="small" onClick={() => onChange([...conditions, { metric: metrics[0] ?? "", comparison: ">", value: "" }])}>
          {t("rules.editor.andAlso")}
        </Button>
      )}
    </fieldset>
  );
}

const time = (ms: number, now: Date) => formatWhen(new Date(ms), now);

/** Where the preview replays: one server, or the servers of a site. */
export type PreviewTarget = { kind: "server"; id: string } | { kind: "site"; id: string };

type Loaded = { server: ServerPreview } | { site: SitePreview };

/**
 * "What this rule would have done": the history replayed through the
 * engine's code, refreshed as the fields change. `request` is null while a
 * field is wrong.
 */
export function RulePreview({ target, request, metric }: { target: PreviewTarget; request: ThresholdRequest | null; metric: string }) {
  const [days, setDays] = useState<number>(3);
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const key = request ? JSON.stringify(request) : null;
  useEffect(() => {
    if (!key) return;
    const controller = new AbortController();
    const rule = JSON.parse(key) as ThresholdRequest;
    const timer = setTimeout(async () => {
      setBusy(true);
      try {
        const result: Loaded =
          target.kind === "server"
            ? { server: await api.previewServerThreshold(target.id, rule, days, controller.signal) }
            : { site: await api.previewSiteThreshold(target.id, rule, days, controller.signal) };
        setLoaded(result);
        setFailure(null);
      } catch (e) {
        if (!controller.signal.aborted) setFailure(t("rules.preview.unavailable", { error: describeError(e) }));
      } finally {
        if (!controller.signal.aborted) setBusy(false);
      }
    }, PREVIEW_DELAY_MS);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [key, days, target.kind, target.id]);

  const now = new Date();
  const format = (v: number) => judgedValue(metric, request ?? undefined, v);
  return (
    <section className={styles.preview} aria-label={t("rules.preview.title")} aria-busy={busy}>
      <div className={styles.row}>
        <strong>{t("rules.preview.title")}</strong>
        <div role="group" aria-label={t("rules.preview.period")}>
          {PERIODS.map((d) => (
            <Button key={d} size="small" aria-pressed={d === days} onClick={() => setDays(d)}>
              {t("format.days", { count: d })}
            </Button>
          ))}
        </div>
      </div>
      {!request ? (
        <p className={admin.dim}>{t("rules.preview.invalid")}</p>
      ) : failure ? (
        <p role="alert" className={admin.hintError}>
          {failure}
        </p>
      ) : !loaded ? (
        <p className={admin.dim}>{t("rules.preview.loading")}</p>
      ) : "server" in loaded ? (
        <ServerResult preview={loaded.server} now={now} format={format} request={request} />
      ) : (
        <SiteResult preview={loaded.site} now={now} />
      )}
    </section>
  );
}

function IncidentLines({ incidents, now }: { incidents: ServerPreview["series"][number]["incidents"]; now: Date }) {
  return (
    <ul className={styles.previewList}>
      {incidents.map((i) => (
        <li key={i.opened_at}>
          {i.resolved_at === null
            ? t("rules.preview.stillOpen", { severity: SEVERITY_LABELS[i.severity], from: time(i.opened_at, now) })
            : t("rules.preview.incident", {
                severity: SEVERITY_LABELS[i.severity],
                from: time(i.opened_at, now),
                to: sameDay(i.opened_at, i.resolved_at) ? formatHourMinute(new Date(i.resolved_at)) : time(i.resolved_at, now),
              })}
        </li>
      ))}
    </ul>
  );
}

const sameDay = (a: number, b: number) => new Date(a).toDateString() === new Date(b).toDateString();

function ServerResult({
  preview,
  now,
  format,
  request,
}: {
  preview: ServerPreview;
  now: Date;
  format: (v: number) => string;
  request: ThresholdRequest;
}) {
  if (preview.series.length === 0) return <p className={admin.dim}>{t("rules.preview.noSeries")}</p>;
  const count = preview.series.reduce((n, s) => n + s.incidents.length, 0);
  const several = preview.series.length > 1;
  const lines = (["warning", "critical"] as const).flatMap((severity) => {
    const value = request[severity];
    return value === null || value === undefined ? [] : [{ severity, value, label: SEVERITY_LABELS[severity] }];
  });
  return (
    <>
      <p>{count === 0 ? t("rules.preview.none") : t("rules.preview.count", { count })}</p>
      <TimeChart
        title={t("rules.preview.chart")}
        series={preview.series.slice(0, 8).map((s) => ({
          label: s.instance || metricTitle(request.metric),
          points: s.points.map(([ms, v]) => [ms / 1000, v] as [number, number]),
        }))}
        start={preview.from / 1000}
        end={preview.to / 1000}
        format={format}
        thresholds={lines}
        emptyText={t("rules.preview.noSeries")}
      />
      {preview.series
        .filter((s) => s.incidents.length > 0)
        .map((s) => (
          <div key={s.instance}>
            {several && <span className={admin.dim}>{t("rules.preview.on", { instance: s.instance })}</span>}
            <IncidentLines incidents={s.incidents} now={now} />
          </div>
        ))}
      {preview.truncated && <p className={admin.dim}>{t("rules.preview.truncated", { count: preview.series.length })}</p>}
    </>
  );
}

function SiteResult({ preview, now }: { preview: SitePreview; now: Date }) {
  const count = preview.servers.reduce((n, s) => n + s.incidents, 0);
  const hit = preview.servers.filter((s) => s.incidents > 0);
  return (
    <>
      <p>
        {count === 0
          ? t("rules.preview.none")
          : t("rules.preview.site", { count, servers: hit.length, total: preview.servers.length })}
      </p>
      {hit.map((s) => (
        <div key={s.server_id}>
          <span className={admin.dim}>{s.hostname}</span>
          <IncidentLines incidents={s.series.flatMap((x) => x.incidents)} now={now} />
        </div>
      ))}
      {preview.more_servers && <p className={admin.dim}>{t("rules.preview.moreServers", { count: preview.servers.length })}</p>}
    </>
  );
}

/**
 * Rules MonitorFlow suggests for the site (ADR-0153), with Add and No
 * thanks, which writes the same rule turned off so the offer stops.
 */
export function Recommendations({
  recommended,
  site,
  siteName,
  canEdit,
  onChanged,
}: {
  recommended: RecommendedThreshold[];
  site: ConfigOwner;
  siteName: string;
  canEdit: boolean;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (recommended.length === 0) return null;
  async function write(rule: ThresholdRequest) {
    setBusy(true);
    setError(null);
    try {
      await api.createThreshold(site, rule);
      onChanged();
    } catch (e) {
      setError(t("rules.recommend.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }
  return (
    <ul className={styles.list}>
      {recommended.map((r) => (
        <li key={r.key} className={cx(styles.item, styles.recommended)}>
          <div className={styles.row}>
            <span className={styles.text}>
              <strong>{t(`rules.recommend.${r.key as "disk_forecast"}`)}</strong>{" "}
              <span className={admin.dim}>{t(`rules.recommend.${r.key as "disk_forecast"}Text`, { site: siteName })}</span>
            </span>
            {canEdit && (
              <span className={styles.actions}>
                <Button size="small" variant="k" disabled={busy} onClick={() => void write(r.rule)}>
                  {t("rules.recommend.add")}
                </Button>
                <Button size="small" disabled={busy} onClick={() => void write({ ...r.rule, enabled: false })}>
                  {t("rules.recommend.decline")}
                </Button>
              </span>
            )}
          </div>
          {error && (
            <span role="alert" className={admin.hintError}>
              {error}
            </span>
          )}
        </li>
      ))}
    </ul>
  );
}
