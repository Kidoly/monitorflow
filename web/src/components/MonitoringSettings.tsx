"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useId, useState } from "react";

import {
  ApiError,
  type ConfigOwner,
  type EffectiveSensor,
  type EffectiveThreshold,
  type SensorConfigEntry,
  api,
  describeError,
} from "@/lib/api";
import {
  type CheckType,
  type FormErrors,
  type Shape,
  type ThresholdForm,
  SEVERITIES,
  checkErrors,
  checkRequests,
  checkTitle,
  checkTypesFor,
  describeThreshold,
  formOf,
  saveThreshold,
  shapeFor,
  thresholdRequest,
} from "@/lib/alerts";
import { formatSeconds } from "@/lib/format";
import { METRIC_PREFIX } from "@/lib/metrics";
import { siteHref } from "@/lib/routes";
import { type Severity } from "@/lib/thresholds";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import styles from "./MonitoringSettings.module.css";

/** Whose settings the panel shows: one server of a site, or a whole site. */
export type Target =
  | { kind: "server"; id: string; siteId: string; siteName: string; osFamily: string }
  | { kind: "site"; id: string; siteName: string };

interface Data {
  thresholds: EffectiveThreshold[];
  sensors: EffectiveSensor[];
  /** The target's own rows, for the checks it turned off. */
  rows: SensorConfigEntry[];
}

const ownerOf = (target: Target): ConfigOwner => ({ kind: target.kind === "server" ? "servers" : "sites", id: target.id });
const siteOwner = (target: Target): ConfigOwner => ({ kind: "sites", id: target.kind === "server" ? target.siteId : target.id });

async function load(target: Target, signal: AbortSignal): Promise<Data> {
  const server = target.kind === "server";
  const [thresholds, sensors, rows] = await Promise.all([
    (server ? api.serverThresholds(target.id, signal) : api.siteThresholds(target.id, signal)).then((t) => t.thresholds),
    (server ? api.serverSensors(target.id, signal) : api.siteSensors(target.id, signal)).then((s) => s.sensors),
    api.sensorRows(ownerOf(target), signal).then((r) => r.entries),
  ]);
  return { thresholds, sensors, rows };
}

/**
 * The alerts and checks of a server or a site, in plain words, with what
 * each value comes from (ADR-0057). Site values are defaults that a
 * server's own values replace; `canEdit` shows the controls, the API
 * decides.
 */
export function MonitoringSettings({ target, canEdit }: { target: Target; canEdit: boolean }) {
  const fetcher = useCallback((signal: AbortSignal) => load(target, signal), [target]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  if (!data || !updatedAt) {
    return error ? (
      <ErrorState what="the alerts and checks" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading alerts and checks" />
    );
  }
  return (
    <div className={styles.panel}>
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {!canEdit && <Banner tone="info">You can see these settings. Changing them needs an operator role on this site.</Banner>}
      <Alerts target={target} data={data} canEdit={canEdit} onChanged={refresh} />
      <Checks target={target} data={data} canEdit={canEdit} onChanged={refresh} />
    </div>
  );
}

/** "Built-in", "Site default", "This server only": where a value comes from. */
function Origin({ target, origin }: { target: Target; origin: EffectiveThreshold["origin"] }) {
  if (origin === "default") return <Label tone="neutral">Built-in</Label>;
  if (target.kind === "site") return <Label tone="info">Set for this site</Label>;
  return origin === "site" ? <Label tone="neutral">Site default</Label> : <Label tone="info">This server only</Label>;
}

function Alerts({ target, data, canEdit, onChanged }: { target: Target; data: Data; canEdit: boolean; onChanged: () => void }) {
  const titleId = useId();
  const [editing, setEditing] = useState<string | null>(null);
  const keyOf = (t: EffectiveThreshold) => `${t.metric}/${t.instance ?? "*"}`;
  return (
    <section className={admin.section} aria-labelledby={titleId}>
      <div className={admin.sectionHead}>
        <h2 id={titleId} className={admin.sectionTitle}>
          When to alert
        </h2>
        <span className={admin.dim}>
          {target.kind === "server"
            ? `Values set for this server replace the defaults of ${target.siteName}.`
            : `Every server of ${target.siteName} uses these unless it has its own value.`}
        </span>
      </div>
      {data.thresholds.length === 0 ? (
        <p className={admin.sectionNote}>No alerts. Add a check below and it comes with one.</p>
      ) : (
        <ul className={styles.list}>
          {data.thresholds.map((rule) => {
            const key = keyOf(rule);
            const shape = shapeFor(data.sensors, rule);
            return (
              <li key={key} className={styles.item}>
                <div className={styles.row}>
                  <span className={cx(styles.text, !rule.enabled && admin.dim)}>{describeThreshold(shape, rule)}</span>
                  <Origin target={target} origin={rule.origin} />
                  {canEdit && editing !== key && (
                    <AlertActions
                      target={target}
                      rule={rule}
                      onEdit={() => setEditing(key)}
                      onChanged={onChanged}
                    />
                  )}
                </div>
                {editing === key && (
                  <ThresholdEditor
                    target={target}
                    rule={rule}
                    shape={shape}
                    onDone={() => {
                      setEditing(null);
                      onChanged();
                    }}
                    onCancel={() => setEditing(null)}
                  />
                )}
              </li>
            );
          })}
        </ul>
      )}
    </section>
  );
}

/** Edit, and going back to the inherited value when the target has its own. */
function AlertActions({
  target,
  rule,
  onEdit,
  onChanged,
}: {
  target: Target;
  rule: EffectiveThreshold;
  onEdit: () => void;
  onChanged: () => void;
}) {
  const [error, setError] = useState<string | null>(null);
  const own = rule.threshold_id !== null && rule.origin === (target.kind === "server" ? "server" : "site");
  async function reset() {
    setError(null);
    try {
      await api.deleteThreshold(rule.threshold_id!);
      onChanged();
    } catch (e) {
      setError(`It was not changed. ${describeError(e)}`);
    }
  }
  const inherited = target.kind === "server" ? "the site default" : "the built-in default";
  return (
    <span className={styles.actions}>
      {rule.enabled ? (
        <Button size="small" onClick={onEdit}>
          Edit
        </Button>
      ) : own ? (
        <Button size="small" onClick={() => void reset()}>
          Turn back on
        </Button>
      ) : (
        target.kind === "server" && (
          <span className={admin.dim}>
            Turned off for the <Link href={siteHref(target.siteId)}>whole site</Link>
          </span>
        )
      )}
      {own && rule.enabled && (
        <Button size="small" onClick={() => void reset()}>
          Use {inherited}
        </Button>
      )}
      {error && (
        <span role="alert" className={admin.hintError}>
          {error}
        </span>
      )}
    </span>
  );
}

/** The inline form under an alert: its levels or severity, its duration, and where it applies. */
function ThresholdEditor({
  target,
  rule,
  shape,
  onDone,
  onCancel,
}: {
  target: Target;
  rule: EffectiveThreshold;
  shape: Shape;
  onDone: () => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<ThresholdForm>(() => formOf(shape, rule));
  const [scope, setScope] = useState<"own" | "site">("own");
  const [errors, setErrors] = useState<FormErrors>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const owner = scope === "site" ? siteOwner(target) : ownerOf(target);

  async function write(request: Parameters<typeof saveThreshold>[1]) {
    setBusy(true);
    setFailure(null);
    try {
      await saveThreshold(owner, request);
      onDone();
    } catch (e) {
      setFailure(`It was not saved. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    const result = thresholdRequest(shape, rule, form);
    if ("errors" in result) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    void write(result.request);
  }

  const set = (key: keyof ThresholdForm) => (value: string) => setForm((f) => ({ ...f, [key]: value }));
  const unit = shape.type === "percent" ? "%" : undefined;
  return (
    <form className={styles.editor} onSubmit={submit} noValidate aria-label="Edit alert">
      <div className={styles.fields}>
        {shape.type === "event" ? (
          <label className={admin.field}>
            Raise
            <select
              className={fieldClass}
              value={form.severity}
              onChange={(e) => set("severity")(e.target.value as Severity)}
            >
              {SEVERITIES.map((s) => (
                <option key={s.value} value={s.value}>
                  {s.label}
                </option>
              ))}
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
      {target.kind === "server" && (
        <fieldset className={styles.scope}>
          <legend>Apply to</legend>
          <label>
            <input type="radio" name="scope" checked={scope === "own"} onChange={() => setScope("own")} /> This server only
          </label>
          <label>
            <input type="radio" name="scope" checked={scope === "site"} onChange={() => setScope("site")} /> Every server of{" "}
            {target.siteName}
            {rule.origin === "server" && <span className={admin.dim}> (this server keeps its own value)</span>}
          </label>
        </fieldset>
      )}
      <div className={styles.buttons}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? "Saving…" : "Save"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        <Button
          variant="danger"
          disabled={busy}
          onClick={() =>
            void write({
              metric: rule.metric,
              instance: rule.instance,
              comparison: rule.comparison,
              warning: rule.warning,
              critical: rule.critical,
              duration_s: rule.duration_s,
              hysteresis: rule.hysteresis,
              enabled: false,
            })
          }
        >
          Turn this alert off
        </Button>
        {failure && (
          <span role="alert" className={admin.hintError}>
            {failure}
          </span>
        )}
      </div>
    </form>
  );
}

function Field({
  label,
  unit,
  value,
  onChange,
  error,
  hint,
}: {
  label: string;
  unit?: string;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  hint?: string;
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
          aria-invalid={error ? true : undefined}
          aria-describedby={hintId}
        />
        {unit && <span>{unit}</span>}
      </span>
      <span id={hintId} className={error ? styles.error : admin.fieldNote} role={error ? "alert" : undefined}>
        {error ?? hint}
      </span>
    </div>
  );
}

function Checks({ target, data, canEdit, onChanged }: { target: Target; data: Data; canEdit: boolean; onChanged: () => void }) {
  const titleId = useId();
  const [adding, setAdding] = useState(false);
  const layer = target.kind === "server" ? "server" : "site";
  const off = data.rows.filter((r) => !r.enabled);
  const here = target.kind === "server" ? "this server" : "this site";

  async function remove(sensor: EffectiveSensor) {
    const owner = ownerOf(target);
    if (sensor.origin === layer && sensor.entry_id) {
      // A check added here goes with the alerts set here on its metrics.
      const prefix = `${METRIC_PREFIX}${sensor.name}_`;
      const { entries } = await api.thresholdRows(owner);
      await Promise.all(entries.filter((e) => e.metric.startsWith(prefix)).map((e) => api.deleteThreshold(e.id)));
      await api.deleteSensor(sensor.entry_id);
    } else {
      await api.createSensor(owner, {
        kind: sensor.kind,
        name: sensor.name,
        instance: sensor.instance,
        interval_ms: sensor.interval_ms,
        params: sensor.params,
        enabled: false,
      });
    }
    onChanged();
  }

  return (
    <section className={admin.section} aria-labelledby={titleId}>
      <div className={admin.sectionHead}>
        <h2 id={titleId} className={admin.sectionTitle}>
          What is checked
        </h2>
        <span className={admin.dim}>Every check runs each minute unless set otherwise.</span>
        {canEdit && !adding && (
          <Button variant="k" size="small" className={styles.push} onClick={() => setAdding(true)}>
            Add a check
          </Button>
        )}
      </div>
      {adding && (
        <AddCheck
          target={target}
          onDone={() => {
            setAdding(false);
            onChanged();
          }}
          onCancel={() => setAdding(false)}
        />
      )}
      {data.sensors.length === 0 && off.length === 0 ? (
        <p className={admin.sectionNote}>Nothing is checked. Use Add a check to watch a port, a service, a log or a script.</p>
      ) : (
        <ul className={styles.list}>
          {data.sensors.map((sensor) => {
            const own = sensor.origin === layer;
            const title = checkTitle(sensor);
            return (
              <li key={`${sensor.name}/${sensor.instance}`} className={cx(styles.item, styles.row)}>
                <span className={styles.text}>
                  {title} <span className={admin.dim}>· every {formatSeconds(sensor.interval_ms / 1000)}</span>
                </span>
                <Origin target={target} origin={sensor.origin} />
                {canEdit && (
                  <span className={styles.actions}>
                    <InlineConfirm
                      layout="row"
                      trigger={own ? "Remove" : "Turn off"}
                      triggerName={`${own ? "Remove" : "Turn off"} ${title}`}
                      title={own ? `Remove ${title} and its alert?` : `Stop checking ${title} on ${here}?`}
                      consequence={own ? "" : "You can turn it back on here."}
                      confirmLabel={own ? "Remove" : "Turn off"}
                      pendingLabel="Saving…"
                      onConfirm={() => remove(sensor)}
                      describeError={(e) => `It was not changed. ${describeError(e)}`}
                    />
                  </span>
                )}
              </li>
            );
          })}
          {off.map((row) => (
            <li key={row.id} className={cx(styles.item, styles.row)}>
              <span className={cx(styles.text, admin.dim)}>
                {checkTitle({ ...row, origin: layer, entry_id: row.id, params: stringParams(row.params) })}: turned off for {here}
              </span>
              {canEdit && (
                <span className={styles.actions}>
                  <TurnBackOn id={row.id} onChanged={onChanged} />
                </span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function TurnBackOn({ id, onChanged }: { id: string; onChanged: () => void }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <Button
        size="small"
        onClick={() =>
          void api.deleteSensor(id).then(onChanged, (e: unknown) => setError(`It was not changed. ${describeError(e)}`))
        }
      >
        Turn back on
      </Button>
      {error && (
        <span role="alert" className={admin.hintError}>
          {error}
        </span>
      )}
    </>
  );
}

/** The Add a check form: a kind, its few fields prefilled where possible, and where it applies. */
function AddCheck({ target, onDone, onCancel }: { target: Target; onDone: () => void; onCancel: () => void }) {
  const types = checkTypesFor(target.kind === "server" ? target.osFamily : null);
  const [type, setType] = useState<CheckType>(types[0]!);
  const [values, setValues] = useState<Record<string, string>>(() => initialValues(types[0]!));
  const [scope, setScope] = useState<"own" | "site">("own");
  const [alert, setAlert] = useState(true);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  function choose(kind: string) {
    const next = types.find((t) => t.kind === kind)!;
    setType(next);
    setValues(initialValues(next));
    setErrors({});
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = checkErrors(type, values);
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    const owner = scope === "site" ? siteOwner(target) : ownerOf(target);
    const { sensor, threshold } = checkRequests(type, values);
    setBusy(true);
    setFailure(null);
    try {
      await api.createSensor(owner, sensor);
      if (alert) await saveThreshold(owner, threshold);
      onDone();
    } catch (e) {
      setFailure(
        e instanceof ApiError && e.status === 409
          ? "This check already exists here."
          : `The check was not added. ${describeError(e)}`,
      );
    } finally {
      setBusy(false);
    }
  }

  const severity = type.severity === "critical" ? "a critical alert" : "a warning";
  return (
    <form className={cx(styles.editor, styles.add)} onSubmit={(e) => void submit(e)} noValidate aria-label="Add a check">
      <div className={styles.fields}>
        <label className={admin.field}>
          Check that
          <select className={fieldClass} value={type.kind} onChange={(e) => choose(e.target.value)}>
            {types.map((t) => (
              <option key={t.kind} value={t.kind}>
                {t.label}
              </option>
            ))}
          </select>
        </label>
        {type.fields.map((field) => (
          <TextField
            key={`${type.kind}/${field.key}`}
            label={field.label}
            placeholder={field.placeholder}
            hint={field.hint}
            value={values[field.key] ?? ""}
            error={errors[field.key]}
            onChange={(value) => setValues((v) => ({ ...v, [field.key]: value }))}
          />
        ))}
      </div>
      <label className={styles.check}>
        <input type="checkbox" checked={alert} onChange={(e) => setAlert(e.target.checked)} /> Raise {severity} when it fails
      </label>
      {target.kind === "server" && (
        <fieldset className={styles.scope}>
          <legend>Add to</legend>
          <label>
            <input type="radio" name="add-scope" checked={scope === "own"} onChange={() => setScope("own")} /> This server only
          </label>
          <label>
            <input type="radio" name="add-scope" checked={scope === "site"} onChange={() => setScope("site")} /> Every server of{" "}
            {target.siteName}
          </label>
        </fieldset>
      )}
      <div className={styles.buttons}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? "Adding…" : "Add check"}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          Cancel
        </Button>
        {failure && (
          <span role="alert" className={admin.hintError}>
            {failure}
          </span>
        )}
      </div>
    </form>
  );
}

const stringParams = (params: SensorConfigEntry["params"]) =>
  Object.fromEntries(Object.entries(params).map(([key, value]) => [key, String(value)]));

const initialValues = (type: CheckType) => Object.fromEntries(type.fields.map((f) => [f.key, f.initial ?? ""]));

function TextField({
  label,
  placeholder,
  hint,
  value,
  error,
  onChange,
}: {
  label: string;
  placeholder: string;
  hint?: string;
  value: string;
  error?: string;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const hintId = `${id}-hint`;
  return (
    <div className={admin.field}>
      <label htmlFor={id}>{label}</label>
      <input
        id={id}
        className={cx(fieldClass, error && admin.fieldError)}
        value={value}
        placeholder={placeholder}
        spellCheck={false}
        autoCapitalize="none"
        onChange={(e) => onChange(e.target.value)}
        aria-invalid={error ? true : undefined}
        aria-describedby={hintId}
      />
      <span id={hintId} className={error ? styles.error : admin.fieldNote} role={error ? "alert" : undefined}>
        {error ?? hint ?? " "}
      </span>
    </div>
  );
}
