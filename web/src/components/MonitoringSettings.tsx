"use client";

import Link from "next/link";
import { type FormEvent, useCallback, useId, useMemo, useState } from "react";

import {
  ApiError,
  type CheckIntervalOf,
  type ConfigOwner,
  type EffectiveSensor,
  type EffectiveThreshold,
  type RecommendedThreshold,
  type SensorConfigEntry,
  api,
  describeError,
} from "@/lib/api";
import { rich, t } from "@/i18n";
import {
  type CheckField,
  type CheckType,
  DEVICE_CHECK_TYPES,
  type Shape,
  SEVERITIES,
  checkErrors,
  checkRequests,
  checkTitle,
  formOf,
  saveCheckInterval,
  saveThreshold,
  shapeFor,
} from "@/lib/alerts";
import { type CatalogueEntry, CATALOGUE, entriesFor, mostUsed, pluginEntries } from "@/lib/catalogue";
import { type RuleErrors, type RuleForm, describeRule, formOfRule, kindOf, requestOf, ruleRequest } from "@/lib/rules";
import { every, slowAlertNote } from "@/lib/intervals";
import { METRIC_PREFIX, sensorInterval } from "@/lib/metrics";
import { siteHref } from "@/lib/routes";
import { type Severity } from "@/lib/thresholds";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, ErrorState, LoadingBlock, StaleBanner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { Switch } from "@/components/ui/Switch";

import { ProbeSelect, probeLabel, probeValue, useProbes } from "./AddDevice";
import { CheckRecommendations } from "./CheckRecommendations";
import { ConditionFields, KindFields, KindSelect, Recommendations, RulePreview } from "./RuleEditor";
import { CheckCatalogue } from "./CheckCatalogue";
import { type Inherit, IntervalControl, IntervalSelect } from "./IntervalSelect";

import admin from "./AdminPages.module.css";
import styles from "./MonitoringSettings.module.css";

/** Whose settings the panel shows: one server of a site, or a whole site. */
export type Target =
  | { kind: "server"; id: string; siteId: string; siteName: string; osFamily: string }
  | { kind: "site"; id: string; siteName: string };

interface Data {
  thresholds: EffectiveThreshold[];
  /** Rules MonitorFlow suggests for the site (ADR-0153). */
  recommended: RecommendedThreshold[];
  sensors: EffectiveSensor[];
  /** The target's own rows, for the checks it turned off. */
  rows: SensorConfigEntry[];
  /** How often the site's built-in checks run (ADR-0097). */
  interval: CheckIntervalOf;
  /** For a server, what its site runs: the checks it inherits. */
  siteSensors: EffectiveSensor[] | null;
}

const ownerOf = (target: Target): ConfigOwner => ({ kind: target.kind === "server" ? "servers" : "sites", id: target.id });
const siteOwner = (target: Target): ConfigOwner => ({ kind: "sites", id: target.kind === "server" ? target.siteId : target.id });

async function load(target: Target, signal: AbortSignal): Promise<Data> {
  const server = target.kind === "server";
  const [{ thresholds, recommended }, effective, rows, site] = await Promise.all([
    server ? api.serverThresholds(target.id, signal) : api.siteThresholds(target.id, signal),
    server ? api.serverSensors(target.id, signal) : api.siteSensors(target.id, signal),
    api.sensorRows(ownerOf(target), signal).then((r) => r.entries),
    server ? api.siteSensors(target.siteId, signal).then((s) => s.sensors) : null,
  ]);
  const { check_interval_ms, check_interval_origin } = effective;
  return {
    thresholds,
    recommended: recommended ?? [],
    sensors: effective.sensors,
    rows,
    interval: { check_interval_ms, check_interval_origin },
    siteSensors: site,
  };
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
      <ErrorState what={t("monitoring.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("monitoring.loading")} />
    );
  }
  return (
    <div className={styles.panel}>
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {!canEdit && <Banner tone="info">{t("monitoring.readOnly")}</Banner>}
      <Alerts target={target} data={data} canEdit={canEdit} onChanged={refresh} />
      <Checks target={target} data={data} canEdit={canEdit} onChanged={refresh} />
    </div>
  );
}

/**
 * "Site default", "This server only": where a value comes from. Built-in
 * values say nothing: only what differs from the defaults is marked.
 */
function Origin({ target, origin }: { target: Target; origin: EffectiveThreshold["origin"] }) {
  if (origin === "default") return null;
  if (target.kind === "site") return <Label tone="info">{t("monitoring.origin.site")}</Label>;
  return origin === "site" ? (
    <Label tone="neutral">{t("monitoring.origin.siteDefault")}</Label>
  ) : (
    <Label tone="info">{t("monitoring.origin.server")}</Label>
  );
}

function Alerts({ target, data, canEdit, onChanged }: { target: Target; data: Data; canEdit: boolean; onChanged: () => void }) {
  const titleId = useId();
  const [editing, setEditing] = useState<string | null>(null);
  const keyOf = (t: EffectiveThreshold) => `${t.metric}/${t.instance ?? "*"}/${kindOf(t)}`;
  const metrics = [...new Set(data.thresholds.map((t) => t.metric))].sort();
  return (
    <section className={admin.section} aria-labelledby={titleId}>
      <div className={admin.sectionHead}>
        <h2 id={titleId} className={admin.sectionTitle}>
          {t("monitoring.whenToAlert")}
        </h2>
        <span className={admin.dim}>
          {target.kind === "server"
            ? t("monitoring.serverReplaces", { site: target.siteName })
            : t("monitoring.siteDefaults", { site: target.siteName })}
        </span>
      </div>
      <Recommendations
        recommended={data.recommended}
        site={siteOwner(target)}
        siteName={target.siteName}
        canEdit={canEdit}
        onChanged={onChanged}
      />
      {data.thresholds.length === 0 ? (
        <p className={admin.sectionNote}>{t("monitoring.noAlerts")}</p>
      ) : (
        <ul className={styles.list}>
          {data.thresholds.map((rule) => {
            const key = keyOf(rule);
            const shape = shapeFor(data.sensors, rule);
            return (
              <li key={key} className={styles.item}>
                <div className={styles.row}>
                  <span className={cx(styles.text, !rule.enabled && admin.dim)}>
                    {describeRule(shape, rule)}
                    {rule.enabled && <SlowNote durationS={rule.duration_s} intervalS={sensorInterval(data.sensors, rule.metric)} />}
                  </span>
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
                    intervalS={sensorInterval(data.sensors, rule.metric)}
                    metrics={metrics}
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

/** When checks are slower than the alert reads: "Checked every 1 h, so…". */
/**
 * "Turn this alert off…", kept apart from Save at the far right of a rule
 * form, and confirmed in place before anything is sent.
 */
export function TurnAlertOff({ disabled, onConfirm }: { disabled: boolean; onConfirm: () => Promise<void> }) {
  return (
    <span className={styles.turnOff}>
      {disabled ? (
        <Button variant="ghost" disabled>
          {t("monitoring.turnAlertOff")}
        </Button>
      ) : (
        <InlineConfirm
          layout="row"
          trigger={t("monitoring.turnAlertOff")}
          triggerVariant="ghost"
          title={t("monitoring.turnAlertOffTitle")}
          consequence={t("monitoring.turnAlertOffConsequence")}
          confirmLabel={t("monitoring.turnAlertOffConfirm")}
          onConfirm={onConfirm}
        />
      )}
    </span>
  );
}

export function SlowNote({ durationS, intervalS }: { durationS: number; intervalS: number | undefined }) {
  const note = slowAlertNote(durationS, intervalS);
  return note && <span className={cx(admin.dim, styles.note)}>{note}</span>;
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
      setError(t("monitoring.notChanged", { error: describeError(e) }));
    }
  }
  const inherited = target.kind === "server" ? t("monitoring.useSiteDefault") : t("monitoring.useBuiltin");
  return (
    <span className={styles.actions}>
      {rule.enabled ? (
        <Button size="small" onClick={onEdit}>
          {t("monitoring.edit")}
        </Button>
      ) : own ? (
        <Button size="small" onClick={() => void reset()}>
          {t("monitoring.turnBackOn")}
        </Button>
      ) : (
        target.kind === "server" && (
          <span className={admin.dim}>
            {rich("monitoring.offForSite", {
              site: <Link href={siteHref(target.siteId)}>{t("monitoring.wholeSite")}</Link>,
            })}
          </span>
        )
      )}
      {own && rule.enabled && (
        <Button size="small" onClick={() => void reset()}>
          {inherited}
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

/**
 * The inline form under an alert: what it judges (ADR-0153), its levels or
 * severity, its duration, conditions joined with "and", where it applies,
 * and a live preview of what it would have done.
 */
function ThresholdEditor({
  target,
  rule,
  shape,
  intervalS,
  metrics,
  onDone,
  onCancel,
}: {
  target: Target;
  rule: EffectiveThreshold;
  shape: Shape;
  /** How often the check behind it runs, if known. */
  intervalS: number | undefined;
  /** Metrics of the target, for conditions. */
  metrics: string[];
  onDone: () => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<RuleForm>(() => formOfRule(shape, rule, formOf(shape, rule)));
  const [scope, setScope] = useState<"own" | "site">("own");
  const [errors, setErrors] = useState<RuleErrors>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const owner = scope === "site" ? siteOwner(target) : ownerOf(target);
  const result = useMemo(() => ruleRequest(shape, rule, form), [shape, rule, form]);
  const another = form.kind !== kindOf(rule);

  async function write(request: Parameters<typeof saveThreshold>[1]) {
    setBusy(true);
    setFailure(null);
    try {
      await saveThreshold(owner, request);
      onDone();
    } catch (e) {
      setFailure(t("monitoring.notSaved", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if ("errors" in result) {
      setErrors(result.errors);
      return;
    }
    setErrors({});
    void write(result.request);
  }

  const set = <K extends keyof RuleForm>(key: K, value: RuleForm[K]) => setForm((f) => ({ ...f, [key]: value }));
  const field = (key: "warning" | "critical" | "minutes") => (value: string) => set(key, value);
  const unit = shape.type === "percent" ? "%" : shape.type === "number" ? shape.unit : undefined;
  const simple = form.kind === "value" || form.kind === "window";
  const preview = target.kind === "server" ? ({ kind: "server", id: target.id } as const) : ({ kind: "site", id: target.id } as const);
  return (
    <form className={styles.editor} onSubmit={submit} noValidate aria-label={t("monitoring.editAlert")}>
      <div className={styles.fields}>
        <KindSelect
          shape={shape}
          value={form.kind}
          onChange={(kind) => {
            setErrors({});
            setForm(formOfRule(shape, rule, formOf(shape, rule), kind));
          }}
        />
        <KindFields shape={shape} form={form} set={set} errors={errors} />
      </div>
      {another && <p className={admin.dim}>{t("rules.editor.addsAnother")}</p>}
      {simple && (
        <div className={styles.fields}>
          {shape.type === "event" && form.kind === "value" ? (
            <label className={admin.field}>
              {t("monitoring.raise")}
              <select
                className={fieldClass}
                value={form.severity}
                onChange={(e) => set("severity", e.target.value as Severity)}
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
              <Field
                label={t("monitoring.warningAt")}
                unit={unit}
                value={form.warning}
                onChange={field("warning")}
                error={errors.warning}
                placeholder={t("monitoring.noWarning")}
              />
              <Field
                label={t("monitoring.criticalAt")}
                unit={unit}
                value={form.critical}
                onChange={field("critical")}
                error={errors.critical}
                placeholder={t("monitoring.noCritical")}
              />
            </>
          )}
        </div>
      )}
      {form.kind !== "nodata" && (
        <>
          <div className={styles.fields}>
            <Field
              label={t("monitoring.forAtLeast")}
              unit={t("format.unit.minute")}
              value={form.minutes}
              onChange={field("minutes")}
              error={errors.minutes}
              placeholder={t("monitoring.firstBadValue")}
            />
          </div>
          <SlowNote durationS={Math.max(0, Number(form.minutes) || 0) * 60} intervalS={intervalS} />
          <ConditionFields
            metrics={metrics}
            conditions={form.conditions}
            onChange={(conditions) => set("conditions", conditions)}
            error={errors.conditions}
          />
        </>
      )}
      {target.kind === "server" && (
        <fieldset className={styles.scope}>
          <legend>{t("monitoring.applyTo")}</legend>
          <label>
            <input type="radio" name="scope" checked={scope === "own"} onChange={() => setScope("own")} />{" "}
            {t("monitoring.origin.server")}
          </label>
          <label>
            <input type="radio" name="scope" checked={scope === "site"} onChange={() => setScope("site")} />{" "}
            {t("monitoring.everyServerOf", { site: target.siteName })}
            {rule.origin === "server" && <span className={admin.dim}> {t("monitoring.keepsOwn")}</span>}
          </label>
        </fieldset>
      )}
      <RulePreview target={preview} request={"request" in result ? result.request : null} metric={rule.metric} />
      <div className={styles.buttons}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("monitoring.saving") : t("monitoring.save")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("monitoring.cancel")}
        </Button>
        {failure && (
          <span role="alert" className={admin.hintError}>
            {failure}
          </span>
        )}
        {!another && <TurnAlertOff disabled={busy} onConfirm={() => write(requestOf(rule, false))} />}
      </div>
    </form>
  );
}

export function Field({
  label,
  unit,
  value,
  onChange,
  error,
  hint,
  placeholder,
}: {
  label: string;
  unit?: string;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  hint?: string;
  /** What an empty field means ("No warning"), shown in the field instead of a line under it. */
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
          aria-describedby={error || hint ? hintId : undefined}
        />
        {unit && <span>{unit}</span>}
      </span>
      {(error || hint) && (
        <span id={hintId} className={error ? styles.error : admin.fieldNote} role={error ? "alert" : undefined}>
          {error ?? hint}
        </span>
      )}
    </div>
  );
}

/**
 * Stops checking `sensor` on the target: a check added there (`own`) goes,
 * with the alerts set there on its metrics; an inherited one gets a
 * turned-off row, which "Turn back on" removes.
 */
export async function stopChecking(target: Target, sensor: EffectiveSensor, own: boolean): Promise<void> {
  const owner = ownerOf(target);
  if (own && sensor.entry_id) {
    const prefix = `${METRIC_PREFIX}${sensor.name}_`;
    const { entries } = await api.thresholdRows(owner);
    await Promise.all(entries.filter((e) => e.metric.startsWith(prefix)).map((e) => api.deleteThreshold(e.id)));
    await api.deleteSensor(sensor.entry_id);
    return;
  }
  await api.createSensor(owner, {
    kind: sensor.kind,
    name: sensor.name,
    instance: sensor.instance,
    interval_ms: sensor.interval_ms,
    params: sensor.params,
    enabled: false,
  });
}

function Checks({ target, data, canEdit, onChanged }: { target: Target; data: Data; canEdit: boolean; onChanged: () => void }) {
  const titleId = useId();
  const [adding, setAdding] = useState(false);
  const layer = target.kind === "server" ? "server" : "site";
  const off = data.rows.filter((r) => !r.enabled);
  const here = target.kind === "server" ? t("monitoring.thisServer") : t("monitoring.thisSite");

  async function remove(sensor: EffectiveSensor) {
    await stopChecking(target, sensor, sensor.origin === layer && sensor.entry_id !== null);
    onChanged();
  }

  return (
    <section className={admin.section} aria-labelledby={titleId}>
      <div className={admin.sectionHead}>
        <h2 id={titleId} className={admin.sectionTitle}>
          {t("monitoring.whatIsChecked")}
        </h2>
        <BuiltinInterval target={target} interval={data.interval} canEdit={canEdit} onChanged={onChanged} />
        {canEdit && !adding && (
          <Button variant="k" size="small" className={styles.push} onClick={() => setAdding(true)}>
            {t("monitoring.addCheck")}
          </Button>
        )}
      </div>
      {adding && (
        <AddCheck
          target={target}
          intervalMs={data.interval.check_interval_ms}
          onDone={() => {
            setAdding(false);
            onChanged();
          }}
          onCancel={() => setAdding(false)}
        />
      )}
      {target.kind === "server" && (
        <CheckRecommendations
          serverId={target.id}
          osFamily={target.osFamily}
          intervalMs={data.interval.check_interval_ms}
          canEdit={canEdit}
          onAdded={onChanged}
        />
      )}
      {data.sensors.length === 0 && off.length === 0 ? (
        <p className={admin.sectionNote}>{t("monitoring.nothingChecked")}</p>
      ) : (
        <ul className={styles.list}>
          {data.sensors.map((sensor) => {
            const own = sensor.origin === layer;
            const title = checkTitle(sensor);
            return (
              <li key={`${sensor.name}/${sensor.instance}`} className={cx(styles.item, styles.row)}>
                <span className={styles.text}>
                  {title}{" "}
                  {canEdit ? (
                    <CheckInterval target={target} data={data} sensor={sensor} title={title} onChanged={onChanged} />
                  ) : (
                    inheritedInterval(target, data, sensor) !== sensor.interval_ms && (
                      <span className={admin.dim}>· {every(sensor.interval_ms)}</span>
                    )
                  )}
                </span>
                <Origin target={target} origin={sensor.origin} />
                {canEdit && (
                  <span className={styles.actions}>
                    <InlineConfirm
                      layout="row"
                      trigger={own ? t("monitoring.remove") : t("monitoring.turnOff")}
                      triggerName={
                        own ? t("monitoring.removeName", { title }) : t("monitoring.turnOffName", { title })
                      }
                      title={
                        own ? t("monitoring.removeTitle", { title }) : t("monitoring.stopTitle", { title, here })
                      }
                      consequence={own ? "" : t("monitoring.turnBackOnHere")}
                      confirmLabel={own ? t("monitoring.remove") : t("monitoring.turnOff")}
                      pendingLabel={t("monitoring.saving")}
                      onConfirm={() => remove(sensor)}
                      describeError={(e) => t("monitoring.notChanged", { error: describeError(e) })}
                    />
                  </span>
                )}
              </li>
            );
          })}
          {off.map((row) => (
            <li key={row.id} className={cx(styles.item, styles.row)}>
              <span className={cx(styles.text, admin.dim)}>
                {t("monitoring.offFor", {
                  title: checkTitle({ ...row, origin: layer, entry_id: row.id, params: stringParams(row.params) }),
                  here,
                })}
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

/** "Built-in checks run every 1 min on every server of Paris", which operators change for the site. */
export function BuiltinInterval({
  target,
  interval,
  canEdit,
  onChanged,
}: {
  target: Target;
  interval: CheckIntervalOf;
  canEdit: boolean;
  onChanged: () => void;
}) {
  const siteId = target.kind === "server" ? target.siteId : target.id;
  const where =
    target.kind === "server" ? t("monitoring.onEveryServerOf", { site: target.siteName }) : t("monitoring.onEveryServerHere");
  if (!canEdit) {
    return (
      <span className={admin.dim}>{t("monitoring.builtinRun", { every: every(interval.check_interval_ms), where })}</span>
    );
  }
  return (
    <span className={cx(admin.dim, styles.builtin)}>
      {rich("monitoring.builtinRun", {
        every: (
          <IntervalControl
            label={t("monitoring.builtinHowOften")}
            value={interval.check_interval_origin === "site" ? interval.check_interval_ms : null}
            inherit={{ ms: BUILTIN_INTERVAL_MS, label: t("monitoring.inherit.builtin") }}
            save={async (ms) => {
              await api.setCheckInterval(siteId, ms);
              onChanged();
            }}
          />
        ),
        where,
      })}
    </span>
  );
}

/** Interval of the built-in checks before any site sets one (ADR-0016). */
const BUILTIN_INTERVAL_MS = 60_000;

const BUILTIN_CHECKS = ["cpu", "memory", "disk", "diskio"];

/**
 * How often one check runs, changed for the target: its own row, or a
 * copy of the inherited check; going back removes the copy.
 */
function CheckInterval({
  target,
  data,
  sensor,
  title,
  onChanged,
}: {
  target: Target;
  data: Data;
  sensor: EffectiveSensor;
  title: string;
  onChanged: () => void;
}) {
  const [open, setOpen] = useState(false);
  const row = data.rows.find((r) => r.enabled && r.name === sensor.name && r.instance === sensor.instance);
  const inherited = inheritedInterval(target, data, sensor);
  const inherit: Inherit | undefined =
    inherited === undefined
      ? undefined
      : {
          ms: inherited,
          label: target.kind === "server" ? t("monitoring.inherit.siteDefault") : t("monitoring.inherit.default"),
        };
  // A check that follows the default says nothing until asked: the default is told once, above.
  if (!row && inherit && !open) {
    return (
      <Button size="small" aria-label={t("monitoring.howOftenCheck", { title })} onClick={() => setOpen(true)}>
        {t("monitoring.changeInterval")}
      </Button>
    );
  }
  return (
    <IntervalControl
      label={t("monitoring.howOftenCheck", { title })}
      value={row || !inherit ? sensor.interval_ms : null}
      inherit={inherit}
      save={async (ms) => {
        await saveCheckInterval(ownerOf(target), sensor, row, ms);
        onChanged();
      }}
    />
  );
}

/** The interval `sensor` gets without the target's own row; undefined when it has none to fall back to. */
function inheritedInterval(target: Target, data: Data, sensor: EffectiveSensor): number | undefined {
  if (target.kind === "server") {
    return data.siteSensors?.find((s) => s.name === sensor.name && s.instance === sensor.instance)?.interval_ms;
  }
  const builtin = BUILTIN_CHECKS.includes(sensor.name) && sensor.kind === sensor.name && sensor.instance === "";
  return builtin ? data.interval.check_interval_ms : undefined;
}

export function TurnBackOn({ id, onChanged }: { id: string; onChanged: () => void }) {
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <Button
        size="small"
        onClick={() =>
          void api
            .deleteSensor(id)
            .then(onChanged, (e: unknown) => setError(t("monitoring.notChanged", { error: describeError(e) })))
        }
      >
        {t("monitoring.turnBackOn")}
      </Button>
      {error && (
        <span role="alert" className={admin.hintError}>
          {error}
        </span>
      )}
    </>
  );
}

/**
 * Add a check: the catalogue, then the form of the chosen check with its
 * few fields prefilled, how often, its alert and where it applies.
 */
export function AddCheck({
  target,
  intervalMs,
  onDone,
  onCancel,
}: {
  target: Target;
  /** Prefilled: how often the built-in checks run. */
  intervalMs: number;
  onDone: () => void;
  onCancel: () => void;
}) {
  const usable = usePolledData(api.usablePlugins, { intervalMs: 300_000 });
  const sites = usePolledData(api.sites, { intervalMs: 300_000 });
  const usage = usePolledData(api.checkUsage, { intervalMs: 300_000 });
  const [chosen, setChosen] = useState<CatalogueEntry | null>(null);
  const siteId = target.kind === "server" ? target.siteId : target.id;
  const customerId = sites.data?.sites.find((s) => s.id === siteId)?.customer_id ?? null;
  const osFamily = target.kind === "server" ? target.osFamily : null;
  const entries = useMemo(
    () => entriesFor([...CATALOGUE, ...pluginEntries(usable.data?.plugins ?? [], siteId, customerId)], osFamily),
    [usable.data, siteId, customerId, osFamily],
  );
  const top = useMemo(() => mostUsed(entries, usage.data?.checks ?? []), [entries, usage.data]);
  const systems = [...new Set(entries.flatMap((e) => e.systems))];
  const [deviceType, setDeviceType] = useState<CheckType | null>(null);

  // A device has no agent: its checks run from the central server or a relay (ADR-0148).
  if (osFamily === "device") {
    return (
      <div className={cx(styles.editor, styles.add)}>
        {deviceType ? (
          <CheckForm
            key={deviceType.label}
            target={target}
            type={deviceType}
            intervalMs={intervalMs}
            onDone={onDone}
            onBack={() => setDeviceType(null)}
            onCancel={onCancel}
          />
        ) : (
          <div className={styles.buttons} role="group" aria-label={t("monitoring.addCheckForm")}>
            {DEVICE_CHECK_TYPES.map((type) => (
              <Button key={type.label} onClick={() => setDeviceType(type)}>
                {type.label}
              </Button>
            ))}
            <Button variant="ghost" onClick={onCancel}>
              {t("monitoring.cancel")}
            </Button>
          </div>
        )}
      </div>
    );
  }

  return (
    <div className={cx(styles.editor, styles.add)}>
      {chosen?.type ? (
        <CheckForm
          key={chosen.id}
          target={target}
          type={chosen.type}
          intervalMs={intervalMs}
          onDone={onDone}
          onBack={() => setChosen(null)}
          onCancel={onCancel}
        />
      ) : (
        <CheckCatalogue entries={entries} mostUsed={top} systems={systems} onChoose={setChosen} onCancel={onCancel} />
      )}
    </div>
  );
}

/**
 * Who runs a check that may run anywhere (ADR-0150): this server's agent,
 * the default, or one of the site's probes.
 */
function RunBySelect({ siteId, value, onChange }: { siteId: string; value: string; onChange: (value: string) => void }) {
  const probes = useProbes(siteId);
  const id = useId();
  return (
    <div className={admin.field}>
      <label htmlFor={id}>{t("devices.check.from")}</label>
      <select id={id} className={fieldClass} value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{t("monitoring.runBy.agent")}</option>
        {probes.map((probe) => (
          <option key={probeValue(probe)} value={probeValue(probe)}>
            {probeLabel(probe)}
          </option>
        ))}
      </select>
      <span className={admin.fieldNote}>{t("monitoring.runBy.hint")}</span>
    </div>
  );
}

/** The form of one catalogue entry. */
function CheckForm({
  target,
  type,
  intervalMs: initialInterval,
  onDone,
  onBack,
  onCancel,
}: {
  target: Target;
  type: CheckType;
  intervalMs: number;
  onDone: () => void;
  onBack: () => void;
  onCancel: () => void;
}) {
  const [intervalMs, setIntervalMs] = useState(initialInterval);
  const [values, setValues] = useState<Record<string, string>>(() => initialValues(type));
  const [scope, setScope] = useState<"own" | "site">("own");
  const [alert, setAlert] = useState(true);
  const [probe, setProbe] = useState("");
  const [errors, setErrors] = useState<Record<string, string>>({});
  const [failure, setFailure] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    const found = checkErrors(type, values);
    setErrors(found);
    if (Object.keys(found).length > 0) return;
    const owner = scope === "site" ? siteOwner(target) : ownerOf(target);
    const { sensor, threshold, more } = checkRequests(type, values, intervalMs);
    if ((type.probe || type.anyRunner) && probe) sensor.probe = probe;
    setBusy(true);
    setFailure(null);
    try {
      await api.createSensor(owner, sensor);
      if (alert && threshold) await saveThreshold(owner, threshold);
      if (alert) for (const rule of more) await saveThreshold(owner, rule);
      onDone();
    } catch (e) {
      setFailure(
        e instanceof ApiError && e.status === 409
          ? t("monitoring.exists")
          : t("monitoring.notAdded", { error: describeError(e) }),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <form className={styles.form} onSubmit={(e) => void submit(e)} noValidate aria-label={t("monitoring.addCheckForm")}>
      <div className={styles.formHead}>
        <h3 className={styles.formTitle}>{type.label}</h3>
        <Button size="small" onClick={onBack} disabled={busy}>
          {t("monitoring.backToCatalogue")}
        </Button>
      </div>
      <div className={styles.fields}>
        {type.fields
          .filter((field) => !field.advanced)
          .map((field) => (
            <FormField
              key={`${type.label}/${field.key}`}
              field={field}
              value={values[field.key] ?? ""}
              error={errors[field.key]}
              onChange={(value) => setValues((v) => ({ ...v, [field.key]: value }))}
            />
          ))}
      </div>
      {type.fields.some((field) => field.advanced) && (
        <details className={styles.more} open={type.fields.some((f) => f.advanced && errors[f.key])}>
          <summary>{t("monitoring.moreOptions")}</summary>
          <div className={styles.fields}>
            {type.fields
              .filter((field) => field.advanced)
              .map((field) => (
                <FormField
                  key={`${type.label}/${field.key}`}
                  field={field}
                  value={values[field.key] ?? ""}
                  error={errors[field.key]}
                  onChange={(value) => setValues((v) => ({ ...v, [field.key]: value }))}
                />
              ))}
          </div>
        </details>
      )}
      <div className={admin.field}>
        <span>{t("monitoring.howOften")}</span>
        <IntervalSelect label={t("monitoring.howOftenToCheck")} value={intervalMs} onChange={(ms) => setIntervalMs(ms ?? initialInterval)} />
      </div>
      {type.noAlert ? (
        <p className={admin.fieldNote}>{t("monitoring.pluginNoAlert")}</p>
      ) : (
        <>
          <Switch
            checked={alert}
            onChange={setAlert}
            label={type.severity === "critical" ? t("monitoring.raiseCritical") : t("monitoring.raiseWarning")}
          />
          {alert && <SlowNote durationS={type.duration_s} intervalS={intervalMs / 1000} />}
        </>
      )}
      {type.probe && target.kind === "server" && (
        <ProbeSelect siteId={target.siteId} value={probe} onChange={setProbe} label={t("devices.check.from")} />
      )}
      {type.anyRunner && target.kind === "server" && (
        <RunBySelect siteId={target.siteId} value={probe} onChange={setProbe} />
      )}
      {target.kind === "server" && !type.probe && !probe && (
        <fieldset className={styles.scope}>
          <legend>{t("monitoring.addTo")}</legend>
          <label>
            <input type="radio" name="add-scope" checked={scope === "own"} onChange={() => setScope("own")} />{" "}
            {t("monitoring.origin.server")}
          </label>
          <label>
            <input type="radio" name="add-scope" checked={scope === "site"} onChange={() => setScope("site")} />{" "}
            {t("monitoring.everyServerOf", { site: target.siteName })}
          </label>
        </fieldset>
      )}
      <div className={styles.buttons}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("monitoring.adding") : t("monitoring.addCheckButton")}
        </Button>
        <Button onClick={onCancel} disabled={busy}>
          {t("monitoring.cancel")}
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

export const stringParams = (params: SensorConfigEntry["params"]) =>
  Object.fromEntries(Object.entries(params).map(([key, value]) => [key, String(value)]));

const initialValues = (type: CheckType) => Object.fromEntries(type.fields.map((f) => [f.key, f.initial ?? ""]));

/** One field of the form, typed as it says: a line, a password, several lines or a choice. */
function FormField({
  field,
  value,
  error,
  onChange,
}: {
  field: CheckField;
  value: string;
  error?: string;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const hintId = `${id}-hint`;
  if (!field.input || field.input === "text") {
    return <TextField label={field.label} placeholder={field.placeholder} hint={field.hint} value={value} error={error} onChange={onChange} />;
  }
  const common = {
    id,
    className: cx(fieldClass, error && admin.fieldError),
    value,
    "aria-invalid": error ? true : undefined,
    "aria-describedby": hintId,
  } as const;
  return (
    <div className={admin.field}>
      <label htmlFor={id}>{field.label}</label>
      {field.input === "select" ? (
        <select {...common} onChange={(e) => onChange(e.target.value)}>
          {field.options?.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>
      ) : field.input === "lines" ? (
        <textarea {...common} rows={3} placeholder={field.placeholder} spellCheck={false} onChange={(e) => onChange(e.target.value)} />
      ) : (
        <input {...common} type="password" autoComplete="new-password" placeholder={field.placeholder} onChange={(e) => onChange(e.target.value)} />
      )}
      <span id={hintId} className={error ? styles.error : admin.fieldNote} role={error ? "alert" : undefined}>
        {error ?? field.hint ?? " "}
      </span>
    </div>
  );
}

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
