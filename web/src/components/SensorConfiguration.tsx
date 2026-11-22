"use client";

import { type FormEvent, useEffect, useRef, useState } from "react";

import { t } from "@/i18n";
import {
  type EffectiveSensor,
  type EffectiveSensors,
  type EffectiveThreshold,
  type SensorConfigEntry,
  type ServerDetail,
  api,
  describeError,
} from "@/lib/api";
import {
  type FormErrors,
  type Shape,
  type ThresholdForm,
  formOf,
  shapeFor,
  shapeOf,
  thresholdRequest,
} from "@/lib/alerts";
import { formatSeconds } from "@/lib/format";
import { type RuleErrors, type RuleForm, describeRule, formOfRule, kindOf, requestOf, ruleRequest } from "@/lib/rules";
import type { RuleKind } from "@/lib/api";
import { defaultTimeoutMs, every } from "@/lib/intervals";
import {
  type ApplyTo,
  type RuleChange,
  type RulePlace,
  LEVEL_LABELS,
  applyToChoices,
  applyToFor,
  instanceNoun,
  ownOverrides,
  perInstance,
  resetRule,
  ruleLevel,
  ruleOf,
  saveRule,
  thresholdLevel,
  timingLevel,
} from "@/lib/sensorRules";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type Fact, FactsBox } from "@/components/ui/FactsBox";
import { missingText } from "@/components/ui/Missing";
import { Label } from "@/components/ui/StatusLabel";
import { SegmentedControl } from "@/components/ui/SegmentedControl";

import admin from "./AdminPages.module.css";
import styles from "./SensorConfiguration.module.css";
import { Field, SlowNote, TurnAlertOff } from "./MonitoringSettings";
import { IntervalSelect } from "./IntervalSelect";
import { ConditionFields, KindFields, KindSelect, RulePreview } from "./RuleEditor";

/** What the sensor page read, for its Configuration card; null where it could not be read. */
export interface ConfigurationData {
  server: ServerDetail;
  config: EffectiveSensors | null;
  rows: SensorConfigEntry[] | null;
  siteSensors: EffectiveSensor[] | null;
  thresholds: EffectiveThreshold[] | null;
}

/**
 * The Configuration card of a sensor page: how often it runs, its timeout
 * and when it alerts, with the level each comes from. Operators of the
 * site edit it in place and choose where the change applies: this sensor
 * only, every instance on this server, or every server of the site
 * (ADR-0016, ADR-0017); "Reset to inherited" removes this server's own.
 */
export function SensorConfiguration({
  data,
  sensor,
  metric,
  instance,
  format,
  canEdit,
  onChanged,
  startEditing = false,
  onEditEnd,
}: {
  data: ConfigurationData;
  sensor: EffectiveSensor | undefined;
  metric: string;
  instance: string;
  format: (v: number) => string;
  canEdit: boolean;
  onChanged: () => void;
  /** Opens the form at once, as the Edit of a server's sensor list does. */
  startEditing?: boolean;
  onEditEnd?: () => void;
}) {
  const [editing, setEditingState] = useState(startEditing);
  const box = useRef<HTMLDivElement>(null);
  const setEditing = (on: boolean) => {
    setEditingState(on);
    if (!on) onEditEnd?.();
  };
  useEffect(() => {
    // Coming from the list's Edit, the form is below the charts: bring it into view.
    if (startEditing) box.current?.scrollIntoView?.({ block: "center" });
  }, [startEditing]);
  const { server, config, thresholds } = data;
  const rule = thresholds ? ruleOf(thresholds, metric, instance) : undefined;
  const richer = thresholds ? richerOf(thresholds, metric, instance) : [];
  const sensors = config?.sensors ?? [];
  const shape = rule ? shapeFor(sensors, rule) : shapeOf(sensors, metric, instance || null);
  const place: RulePlace | null = sensor ? { serverId: server.id, siteId: server.site_id, sensor, instance } : null;
  const editable = canEdit && place !== null && data.rows !== null && data.siteSensors !== null && thresholds !== null;
  const overrides = editable ? ownOverrides(place, rule, data.rows!, data.siteSensors!) : null;

  const actions = editable && !editing && (
    <>
      {overrides?.level && <ResetButton place={place} overrides={overrides} onChanged={onChanged} />}
      <Button size="small" onClick={() => setEditing(true)}>
        {t("sensors.config.edit")}
      </Button>
    </>
  );

  return (
    <div ref={box} id="configuration">
      <FactsBox
        title={t("sensors.config.title")}
        actions={actions}
        facts={[
          ...facts(data, sensor, instance, rule, shape, format),
          ...richer
            .filter((r) => r.enabled)
            .map((r) => ({ label: t("rules.sensor.also"), value: describeRule(shapeOf(sensors, metric, r.instance), r) })),
        ]}
        details={[
          ...(rule ? [marginFact(rule, format)] : []),
          ...(sensor ? [setByFact(sensor, rule, perInstance(sensor, instance))] : []),
        ]}
      >
        {editing && place && (
          <RuleEditor
            place={place}
            hostname={server.hostname}
            siteName={server.site_name}
            rule={rule}
            richer={richer}
            metrics={thresholds ? [...new Set(thresholds.map((t) => t.metric))].sort() : []}
            shape={shape}
            metric={metric}
            hasOwn={overrides?.level !== null && overrides?.level !== undefined}
            onDone={() => {
              setEditing(false);
              onChanged();
            }}
            onCancel={() => setEditing(false)}
          />
        )}
      </FactsBox>
    </div>
  );
}

function facts(
  data: ConfigurationData,
  sensor: EffectiveSensor | undefined,
  instance: string,
  rule: EffectiveThreshold | undefined,
  shape: Shape,
  format: (v: number) => string,
): Fact[] {
  const watches = instance || sensor?.instance;
  return [
    { label: t("sensors.config.kind"), value: sensor?.kind ?? t("sensors.config.unknown") },
    { label: t("sensors.config.watches"), value: watches || missingText(sensor ? "none" : "unknown"), mono: Boolean(watches) },
    { label: t("sensors.config.howOften"), value: sensor ? every(sensor.interval_ms) : missingText("unknown") },
    {
      label: t("sensors.config.timeout"),
      value: sensor
        ? sensor.timeout_ms === null
          ? t("sensors.config.automatic", { duration: formatSeconds(defaultTimeoutMs(sensor.interval_ms) / 1000) })
          : formatSeconds(sensor.timeout_ms / 1000)
        : missingText("unknown"),
    },
    ...alertFacts(rule, shape, format, data.thresholds === null, sensor?.interval_ms),
    ...(sensor && Object.keys(sensor.params).length > 0
      ? [{ label: t("sensors.config.parameters"), value: Object.entries(sensor.params).map(([k, v]) => `${k}=${v}`).join(" "), mono: true }]
      : []),
    ...(sensor ? [{ label: t("sensors.config.appliesFrom"), value: LEVEL_LABELS[ruleLevel(sensor, rule, perInstance(sensor, instance))] }] : []),
    // Up to date is the normal state: only waiting or out of date is a fact worth a line.
    ...(data.config && !data.config.in_sync ? [configurationFact(data.config)] : []),
  ];
}

function alertFacts(
  rule: EffectiveThreshold | undefined,
  shape: Shape,
  format: (v: number) => string,
  failed: boolean,
  intervalMs: number | undefined,
): Fact[] {
  if (!rule) {
    return [{ label: t("sensors.config.thresholds"), value: failed ? t("sensors.config.unreadable") : t("sensors.config.none") }];
  }
  if (!rule.enabled) return [{ label: t("sensors.config.thresholds"), value: t("sensors.config.off") }];
  const duration = {
    label: t("sensors.config.duration"),
    value: (
      <>
        {formatSeconds(rule.duration_s)} <span className={admin.dim}>{t("sensors.config.durationNote")}</span>
        <SlowNote durationS={rule.duration_s} intervalS={intervalMs === undefined ? undefined : intervalMs / 1000} />
      </>
    ),
  };
  if (shape.type === "event") {
    const severity = rule.critical !== null ? t("sensors.config.critical") : t("sensors.config.warning");
    return [{ label: t("sensors.config.alert"), value: `${severity} ${shape.brief}` }, duration];
  }
  return [
    {
      label: t("sensors.config.warning"),
      value: rule.warning === null ? t("sensors.config.noneShort") : `${rule.comparison} ${format(rule.warning)}`,
    },
    {
      label: t("sensors.config.critical"),
      value: rule.critical === null ? t("sensors.config.noneShort") : `${rule.comparison} ${format(rule.critical)}`,
    },
    duration,
  ];
}

function marginFact(rule: EffectiveThreshold, format: (v: number) => string): Fact {
  const above = rule.comparison === ">" || rule.comparison === ">=";
  return {
    label: t("sensors.config.margin"),
    value: (
      <>
        {format(rule.hysteresis)}{" "}
        <span className={admin.dim}>{above ? t("sensors.config.marginBelow") : t("sensors.config.marginAbove")}</span>
      </>
    ),
  };
}

function setByFact(sensor: EffectiveSensor, rule: EffectiveThreshold | undefined, several: boolean): Fact {
  return {
    label: t("sensors.config.setBy"),
    value: (
      <>
        {t("sensors.config.setByHowOften", { level: LEVEL_LABELS[timingLevel(sensor)] })}
        {rule && (
          <span className={admin.dim}>{t("sensors.config.setByThresholds", { level: LEVEL_LABELS[thresholdLevel(rule, several)] })}</span>
        )}
      </>
    ),
  };
}

function configurationFact(config: EffectiveSensors): Fact {
  if (config.applied_revision === null) {
    return { label: t("sensors.config.title"), value: <Label tone="neutral">{t("sensors.config.waiting")}</Label> };
  }
  return {
    label: t("sensors.config.title"),
    value: <Label tone="warn">{t("sensors.config.outOfDate")}</Label>,
  };
}

/** "Reset to inherited": removes this server's own rule at the level in effect. */
function ResetButton({
  place,
  overrides,
  onChanged,
}: {
  place: RulePlace;
  overrides: ReturnType<typeof ownOverrides>;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const to = overrides.level === "sensor" ? t("sensors.reset.toEvery", { noun: instanceNoun(place.sensor) }) : t("sensors.reset.toSite");
  async function reset() {
    setBusy(true);
    setError(null);
    try {
      await resetRule(overrides);
      onChanged();
    } catch (e) {
      setError(t("sensors.reset.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      {error && (
        <span role="alert" className={admin.hintError}>
          {error}
        </span>
      )}
      <Button size="small" disabled={busy} onClick={() => void reset()} title={t("sensors.reset.title", { to })}>
        {busy ? t("sensors.reset.busy") : t("sensors.reset.button")}
      </Button>
    </>
  );
}

/** A threshold to start from when the metric has none: nothing set, compared the way its kind reads. */
function blankRule(metric: string, shape: Shape): EffectiveThreshold {
  const below = shape.type === "event" ? shape.comparison === "<" : /available_ratio$/.test(metric);
  return {
    origin: "default",
    threshold_id: null,
    metric,
    instance: null,
    enabled: true,
    comparison: shape.type === "event" ? shape.comparison : below ? "<" : ">",
    warning: null,
    critical: null,
    duration_s: 300,
    hysteresis: 0,
  };
}

/** The interval and timeout typed, or what is wrong with them. */
function timingOf(intervalMs: number, timeout: string): { interval_ms: number; timeout_ms: number | null } | { error: string } {
  const text = timeout.trim();
  if (text === "") return { interval_ms: intervalMs, timeout_ms: null };
  const seconds = Number(text.replace(",", "."));
  if (!Number.isFinite(seconds) || seconds <= 0 || seconds * 1000 > intervalMs) {
    return { error: t("sensors.edit.timeoutError", { max: formatSeconds(intervalMs / 1000) }) };
  }
  return { interval_ms: intervalMs, timeout_ms: Math.round(seconds * 1000) };
}

/**
 * The rules of other kinds than the plain threshold on this series
 * (ADR-0153), one per kind: the exact instance's wins over the one for
 * every instance.
 */
function richerOf(thresholds: EffectiveThreshold[], metric: string, instance: string): EffectiveThreshold[] {
  const found = new Map<RuleKind, EffectiveThreshold>();
  for (const t of thresholds) {
    const kind = kindOf(t);
    if (kind === "value" || t.metric !== metric) continue;
    if (t.instance === instance) found.set(kind, t);
    else if (t.instance === null && !found.has(kind)) found.set(kind, t);
  }
  return [...found.values()];
}

function RuleEditor({
  place,
  hostname,
  siteName,
  rule,
  richer,
  metrics,
  shape,
  metric,
  hasOwn,
  onDone,
  onCancel,
}: {
  place: RulePlace;
  hostname: string;
  siteName: string;
  rule: EffectiveThreshold | undefined;
  /** The rules of other kinds on this series (ADR-0153). */
  richer: EffectiveThreshold[];
  /** Metrics of the server, for conditions. */
  metrics: string[];
  shape: Shape;
  metric: string;
  /** This server has its own rule, which a site default does not replace. */
  hasOwn: boolean;
  onDone: () => void;
  onCancel: () => void;
}) {
  const { sensor } = place;
  const several = perInstance(sensor, place.instance);
  const base = rule ?? blankRule(metric, shape);
  const initialForm = formOf(shape, base);
  const initialApplyTo = applyToFor(ruleLevel(sensor, rule, several), several);
  const [intervalMs, setIntervalMs] = useState(sensor.interval_ms);
  const [timeout, setTimeoutText] = useState(sensor.timeout_ms === null ? "" : String(sensor.timeout_ms / 1000));
  const [form, setForm] = useState<ThresholdForm>(initialForm);
  const [applyTo, setApplyTo] = useState<ApplyTo>(initialApplyTo);
  const [errors, setErrors] = useState<FormErrors & { timeout?: string }>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const siteServers = useSiteServerCount(place.siteId);
  // What the alert judges (ADR-0153): the plain threshold keeps its form.
  const [kind, setKind] = useState<RuleKind>("value");
  const ofKind = (k: RuleKind) => richer.find((r) => kindOf(r) === k);
  const [rich, setRich] = useState<RuleForm>(() => formOfRule(shape, base, initialForm));
  const [richErrors, setRichErrors] = useState<RuleErrors>({});
  const richBase = ofKind(kind) ?? base;
  const richResult = kind === "value" ? null : ruleRequest(shape, richBase, rich);
  const chooseKind = (k: RuleKind) => {
    setKind(k);
    setRichErrors({});
    const existing = ofKind(k);
    setRich(formOfRule(shape, existing ?? base, formOf(shape, existing ?? base), k));
  };
  const setRichField = <K extends keyof RuleForm>(key: K, value: RuleForm[K]) => setRich((f) => ({ ...f, [key]: value }));

  async function write(change: RuleChange) {
    setBusy(true);
    setFailure(null);
    try {
      await saveRule(place, applyTo, change);
      onDone();
    } catch (e) {
      setFailure(t("sensors.edit.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const found: FormErrors & { timeout?: string } = {};
    const change: RuleChange = {};
    const timing = timingOf(intervalMs, timeout);
    if ("error" in timing) found.timeout = timing.error;
    else if (timing.interval_ms !== sensor.interval_ms || timing.timeout_ms !== sensor.timeout_ms) {
      change.timing = timing;
    }
    // Only what changed is written where it applies, so the rest keeps following what it inherits.
    const formChanged = JSON.stringify(form) !== JSON.stringify(initialForm);
    if (richResult) {
      if ("errors" in richResult) {
        setRichErrors(richResult.errors);
        Object.assign(found, richResult.errors);
      } else change.threshold = richResult.request;
    } else if (formChanged || (rule && !rule.enabled)) {
      const result = thresholdRequest(shape, base, form);
      if ("errors" in result) Object.assign(found, result.errors);
      else change.threshold = result.request;
    }
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    if (!change.timing && !change.threshold) {
      onCancel();
      return;
    }
    void write(change);
  }

  const set = (key: keyof ThresholdForm) => (value: string) => setForm((f) => ({ ...f, [key]: value }));
  const unit = shape.type === "percent" ? "%" : shape.type === "number" ? shape.unit : undefined;
  const choices = applyToChoices(sensor, place.instance, hostname, siteName);
  const timingChanged = intervalMs !== sensor.interval_ms || timeout !== (sensor.timeout_ms === null ? "" : String(sensor.timeout_ms / 1000));

  return (
    <form className={styles.form} onSubmit={submit} noValidate aria-label={t("sensors.edit.label")}>
      <div className={styles.fields}>
        <div className={admin.field}>
          <span>{t("sensors.config.howOften")}</span>
          <IntervalSelect label={t("sensors.edit.howOftenLabel")} value={intervalMs} onChange={(ms) => setIntervalMs(ms ?? sensor.interval_ms)} />
        </div>
        <Field
          label={t("sensors.config.timeout")}
          unit="s"
          value={timeout}
          onChange={setTimeoutText}
          error={errors.timeout}
          placeholder={t("sensors.edit.timeoutHint", { duration: formatSeconds(defaultTimeoutMs(intervalMs) / 1000) })}
        />
      </div>
      <div className={styles.fields}>
        <KindSelect shape={shape} value={kind} onChange={chooseKind} />
        {kind !== "value" && <KindFields shape={shape} form={rich} set={setRichField} errors={richErrors} />}
      </div>
      {kind !== "value" && !ofKind(kind) && <p className={admin.fieldNote}>{t("rules.editor.addsAnother")}</p>}
      {kind === "value" && (
      <div className={styles.fields}>
        {shape.type === "event" ? (
          <label className={admin.field}>
            {t("sensors.config.alert")}
            <select className={fieldClass} value={form.severity} onChange={(e) => set("severity")(e.target.value)}>
              <option value="critical">{`${t("sensors.config.critical")} ${shape.brief}`}</option>
              <option value="warning">{`${t("sensors.config.warning")} ${shape.brief}`}</option>
            </select>
          </label>
        ) : (
          <>
            <Field
              label={t("sensors.edit.warningAt")}
              unit={unit}
              value={form.warning}
              onChange={set("warning")}
              error={errors.warning}
              placeholder={t("sensors.edit.noWarning")}
            />
            <Field
              label={t("sensors.edit.criticalAt")}
              unit={unit}
              value={form.critical}
              onChange={set("critical")}
              error={errors.critical}
              placeholder={t("sensors.edit.noCritical")}
            />
          </>
        )}
        <Field
          label={t("sensors.edit.forAtLeast")}
          unit="min"
          value={form.minutes}
          onChange={set("minutes")}
          error={errors.minutes}
          placeholder={t("sensors.edit.firstValue")}
        />
      </div>
      )}
      {kind !== "value" && kind !== "nodata" && (
        <>
          {kind !== "forecast" && (
            <div className={styles.fields}>
              <Field
                label={t("sensors.edit.forAtLeast")}
                unit="min"
                value={rich.minutes}
                onChange={(v) => setRichField("minutes", v)}
                error={richErrors.minutes}
                placeholder={t("sensors.edit.firstValue")}
              />
            </div>
          )}
          <ConditionFields
            metrics={metrics}
            conditions={rich.conditions}
            onChange={(conditions) => setRichField("conditions", conditions)}
            error={richErrors.conditions}
          />
        </>
      )}
      {rule && !rule.enabled && <p className={admin.fieldNote}>{t("sensors.edit.offNote")}</p>}
      <SlowNote durationS={Math.max(0, Number(form.minutes) || 0) * 60} intervalS={intervalMs / 1000} />
      <fieldset className={styles.applyTo}>
        <legend>{t("sensors.edit.applyTo")}</legend>
        <SegmentedControl
          label={t("sensors.edit.applyTo")}
          options={choices.map((choice) => ({
            value: choice.value,
            label: (
              <>
                {choice.label}
                {(choice.value !== "site" || siteServers !== null) && (
                  <span className={styles.count}>
                    {" · "}
                    {t("sensors.apply.count", { count: choice.value === "site" ? siteServers! : 1 })}
                  </span>
                )}
              </>
            ),
          }))}
          value={applyTo}
          onChange={setApplyTo}
        />
        {applyTo === "site" && hasOwn && (
          <span className={admin.fieldNote}>{t("sensors.edit.keepsOwn")}</span>
        )}
        {applyTo === "sensor" && timingChanged && (
          <span className={admin.fieldNote}>{t("sensors.edit.timingAll", { noun: instanceNoun(sensor), host: hostname })}</span>
        )}
      </fieldset>
      {kind !== "value" && (
        <RulePreview
          target={{ kind: "server", id: place.serverId }}
          request={richResult && "request" in richResult ? richResult.request : null}
          metric={metric}
        />
      )}
      <div className={styles.buttons}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("sensors.edit.saving") : t("sensors.edit.save")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("sensors.edit.cancel")}
        </Button>
        {failure && (
          <span role="alert" className={cx(admin.hintError, styles.failure)}>
            {failure}
          </span>
        )}
        {kind !== "value" && ofKind(kind)?.enabled && (
          <TurnAlertOff disabled={busy} onConfirm={() => write({ threshold: requestOf(ofKind(kind)!, false) })} />
        )}
        {kind === "value" && rule?.enabled && (
          <TurnAlertOff
            disabled={busy}
            onConfirm={() =>
              write({
                threshold: {
                  metric: rule.metric,
                  instance: rule.instance,
                  comparison: rule.comparison,
                  warning: rule.warning,
                  critical: rule.critical,
                  duration_s: rule.duration_s,
                  hysteresis: rule.hysteresis,
                  enabled: false,
                },
              })
            }
          />
        )}
      </div>
    </form>
  );
}

/** How many servers a site default reaches: the site's servers, removed ones aside; null until known. */
function useSiteServerCount(siteId: string): number | null {
  const [count, setCount] = useState<number | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    api.siteServers(siteId, controller.signal).then(
      ({ servers }) => setCount(servers.filter((s) => !s.revoked_at).length),
      () => {},
    );
    return () => controller.abort();
  }, [siteId]);
  return count;
}
