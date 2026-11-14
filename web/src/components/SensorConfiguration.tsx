"use client";

import { type FormEvent, useEffect, useId, useRef, useState } from "react";

import {
  type EffectiveSensor,
  type EffectiveSensors,
  type EffectiveThreshold,
  type SensorConfigEntry,
  type ServerDetail,
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
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import styles from "./SensorConfiguration.module.css";
import { Field, SlowNote } from "./MonitoringSettings";
import { IntervalSelect } from "./IntervalSelect";

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
  const sensors = config?.sensors ?? [];
  const shape = rule ? shapeFor(sensors, rule) : shapeOf(sensors, metric, instance || null);
  const place: RulePlace | null = sensor ? { serverId: server.id, siteId: server.site_id, sensor, instance } : null;
  const editable = canEdit && place !== null && data.rows !== null && data.siteSensors !== null && thresholds !== null;
  const overrides = editable ? ownOverrides(place, rule, data.rows!, data.siteSensors!) : null;

  const actions = editable && !editing && (
    <>
      {overrides?.level && <ResetButton place={place} overrides={overrides} onChanged={onChanged} />}
      <Button size="small" onClick={() => setEditing(true)}>
        Edit
      </Button>
    </>
  );

  return (
    <div ref={box} id="configuration">
      <FactsBox
        title="Configuration"
        actions={actions}
        facts={facts(data, sensor, instance, rule, shape, format)}
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
    { label: "Kind", value: sensor?.kind ?? "unknown" },
    { label: "Watches", value: watches || "—", mono: Boolean(watches) },
    { label: "How often", value: sensor ? every(sensor.interval_ms) : "—" },
    {
      label: "Timeout",
      value: sensor
        ? sensor.timeout_ms === null
          ? `automatic, ${formatSeconds(defaultTimeoutMs(sensor.interval_ms) / 1000)}`
          : formatSeconds(sensor.timeout_ms / 1000)
        : "—",
    },
    ...alertFacts(rule, shape, format, data.thresholds === null, sensor?.interval_ms),
    ...(sensor && Object.keys(sensor.params).length > 0
      ? [{ label: "Parameters", value: Object.entries(sensor.params).map(([k, v]) => `${k}=${v}`).join(" "), mono: true }]
      : []),
    ...(sensor ? [{ label: "Applies from", value: LEVEL_LABELS[ruleLevel(sensor, rule, perInstance(sensor, instance))] }] : []),
    ...(data.config ? [configurationFact(data.config)] : []),
  ];
}

function alertFacts(
  rule: EffectiveThreshold | undefined,
  shape: Shape,
  format: (v: number) => string,
  failed: boolean,
  intervalMs: number | undefined,
): Fact[] {
  if (!rule) return [{ label: "Thresholds", value: failed ? "could not be read" : "none: this sensor never opens incidents" }];
  if (!rule.enabled) return [{ label: "Thresholds", value: "turned off: this sensor opens no incidents" }];
  const duration = {
    label: "Duration",
    value: (
      <>
        {formatSeconds(rule.duration_s)} <span className={admin.dim}>past the threshold before opening</span>
        <SlowNote durationS={rule.duration_s} intervalS={intervalMs === undefined ? undefined : intervalMs / 1000} />
      </>
    ),
  };
  if (shape.type === "event") {
    return [{ label: "Alert", value: `${rule.critical !== null ? "Critical" : "Warning"} ${shape.brief}` }, duration];
  }
  return [
    { label: "Warning", value: rule.warning === null ? "none" : `${rule.comparison} ${format(rule.warning)}` },
    { label: "Critical", value: rule.critical === null ? "none" : `${rule.comparison} ${format(rule.critical)}` },
    duration,
  ];
}

function marginFact(rule: EffectiveThreshold, format: (v: number) => string): Fact {
  const above = rule.comparison === ">" || rule.comparison === ">=";
  return {
    label: "Recovery margin",
    value: (
      <>
        {format(rule.hysteresis)} <span className={admin.dim}>{above ? "below" : "above"} the threshold before resolving</span>
      </>
    ),
  };
}

function setByFact(sensor: EffectiveSensor, rule: EffectiveThreshold | undefined, several: boolean): Fact {
  return {
    label: "Set by",
    value: (
      <>
        How often: {LEVEL_LABELS[timingLevel(sensor)]}
        {rule && <span className={admin.dim}> · thresholds: {LEVEL_LABELS[thresholdLevel(rule, several)]}</span>}
      </>
    ),
  };
}

function configurationFact(config: EffectiveSensors): Fact {
  if (config.applied_revision === null) return { label: "Configuration", value: <Label tone="neutral">Waiting for the agent</Label> };
  return {
    label: "Configuration",
    value: <Label tone={config.in_sync ? "ok" : "warn"}>{config.in_sync ? "Up to date" : "Out of date"}</Label>,
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
  const to = overrides.level === "sensor" ? `every ${instanceNoun(place.sensor)} on this server` : "the site default";
  async function reset() {
    setBusy(true);
    setError(null);
    try {
      await resetRule(overrides);
      onChanged();
    } catch (e) {
      setError(`It was not reset. ${describeError(e)}`);
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
      <Button size="small" disabled={busy} onClick={() => void reset()} title={`Go back to the rule of ${to}`}>
        {busy ? "Resetting…" : "Reset to inherited"}
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
    return { error: `Enter a number of seconds up to ${formatSeconds(intervalMs / 1000)}, or leave it empty.` };
  }
  return { interval_ms: intervalMs, timeout_ms: Math.round(seconds * 1000) };
}

function RuleEditor({
  place,
  hostname,
  siteName,
  rule,
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
  const id = useId();

  async function write(change: RuleChange) {
    setBusy(true);
    setFailure(null);
    try {
      await saveRule(place, applyTo, change);
      onDone();
    } catch (e) {
      setFailure(`It was not saved. ${describeError(e)}`);
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
    if (formChanged || (rule && !rule.enabled)) {
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
    <form className={styles.form} onSubmit={submit} noValidate aria-label="Edit the configuration">
      <div className={styles.fields}>
        <div className={admin.field}>
          <span>How often</span>
          <IntervalSelect label="How often to check" value={intervalMs} onChange={(ms) => setIntervalMs(ms ?? sensor.interval_ms)} />
        </div>
        <Field
          label="Timeout"
          unit="s"
          value={timeout}
          onChange={setTimeoutText}
          error={errors.timeout}
          hint={`Empty: automatic, ${formatSeconds(defaultTimeoutMs(intervalMs) / 1000)}.`}
        />
      </div>
      <div className={styles.fields}>
        {shape.type === "event" ? (
          <label className={admin.field}>
            Alert
            <select className={fieldClass} value={form.severity} onChange={(e) => set("severity")(e.target.value)}>
              <option value="critical">Critical {shape.brief}</option>
              <option value="warning">Warning {shape.brief}</option>
            </select>
          </label>
        ) : (
          <>
            <Field label="Warning at" unit={unit} value={form.warning} onChange={set("warning")} error={errors.warning} hint="Empty: no warning." />
            <Field label="Critical at" unit={unit} value={form.critical} onChange={set("critical")} error={errors.critical} hint="Empty: no critical." />
          </>
        )}
        <Field
          label="For at least"
          unit="min"
          value={form.minutes}
          onChange={set("minutes")}
          error={errors.minutes}
          hint="0 alerts on the first bad value."
        />
      </div>
      {rule && !rule.enabled && <p className={admin.fieldNote}>This alert is turned off. Save turns it back on with these values.</p>}
      <SlowNote durationS={Math.max(0, Number(form.minutes) || 0) * 60} intervalS={intervalMs / 1000} />
      <fieldset className={styles.applyTo}>
        <legend>Apply to</legend>
        {choices.map((choice) => (
          <label key={choice.value}>
            <input
              type="radio"
              name={`${id}-apply`}
              value={choice.value}
              checked={applyTo === choice.value}
              onChange={() => setApplyTo(choice.value)}
            />{" "}
            {choice.label}
          </label>
        ))}
        {applyTo === "site" && hasOwn && (
          <span className={admin.fieldNote}>This server keeps its own rule until you reset it to the inherited one.</span>
        )}
        {applyTo === "sensor" && timingChanged && (
          <span className={admin.fieldNote}>
            How often and the timeout apply to every {instanceNoun(sensor)} on {hostname}: one sensor measures them all.
          </span>
        )}
      </fieldset>
      <div className={styles.buttons}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        {rule?.enabled && (
          <Button
            variant="danger"
            disabled={busy}
            onClick={() =>
              void write({
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
          >
            Turn this alert off
          </Button>
        )}
        {failure && (
          <span role="alert" className={cx(admin.hintError, styles.failure)}>
            {failure}
          </span>
        )}
      </div>
    </form>
  );
}
