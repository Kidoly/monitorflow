"use client";

import Link from "next/link";
import { useState } from "react";

import {
  type EffectiveSensor,
  type EffectiveSensors,
  type EffectiveThreshold,
  type SensorConfigEntry,
  type ServerDetail,
  describeError,
} from "@/lib/api";
import { type Shape, saveCheckInterval, shapeFor, shapeOf } from "@/lib/alerts";
import { formatSeconds } from "@/lib/format";
import { defaultTimeoutMs, every } from "@/lib/intervals";
import { serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import {
  type RulePlace,
  LEVEL_LABELS,
  instanceNoun,
  ownOverrides,
  perInstance,
  resetRule,
  ruleLevel,
  ruleOf,
  thresholdLevel,
  timingLevel,
} from "@/lib/sensorRules";
import { Button } from "@/components/ui/Button";
import { type Fact, FactsBox } from "@/components/ui/FactsBox";
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import { IntervalControl } from "./IntervalSelect";
import { SlowNote } from "./MonitoringSettings";

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
 * and when it alerts, with the level each comes from (ADR-0016,
 * ADR-0017). Operators of the site change how often it runs for this
 * server, and "Reset to inherited" removes this server's own rule.
 */
export function SensorConfiguration({
  data,
  sensor,
  metric,
  instance,
  format,
  canEdit,
  onChanged,
}: {
  data: ConfigurationData;
  sensor: EffectiveSensor | undefined;
  metric: string;
  instance: string;
  format: (v: number) => string;
  canEdit: boolean;
  onChanged: () => void;
}) {
  const scope = useScope();
  const { server, config, thresholds } = data;
  const rule = thresholds ? ruleOf(thresholds, metric, instance) : undefined;
  const sensors = config?.sensors ?? [];
  const shape = rule ? shapeFor(sensors, rule) : shapeOf(sensors, metric, instance || null);
  const place: RulePlace | null = sensor ? { serverId: server.id, siteId: server.site_id, sensor, instance } : null;
  const editable = canEdit && place !== null && data.rows !== null && data.siteSensors !== null && thresholds !== null;
  const overrides = editable ? ownOverrides(place, rule, data.rows!, data.siteSensors!) : null;
  const shown = facts(data, sensor, instance, rule, shape, format).map((fact) =>
    fact.label === "How often" && editable && sensor
      ? { ...fact, value: <IntervalFact data={data} sensor={sensor} onChanged={onChanged} /> }
      : fact,
  );

  return (
    <FactsBox
      title="Configuration"
      actions={overrides?.level && <ResetButton place={place!} overrides={overrides} onChanged={onChanged} />}
      facts={[
        ...shown,
        { label: "Alerts", value: <Link href={scopedHref(serverHref(server.id, "alerts"), scope)}>Change when to alert</Link> },
      ]}
      details={[
        ...(rule ? [marginFact(rule, format)] : []),
        ...(sensor ? [setByFact(sensor, rule, perInstance(sensor, instance))] : []),
      ]}
    />
  );
}

/** How often the check runs, changed for this server: its own row, or going back to the site's (ADR-0097). */
function IntervalFact({ data, sensor, onChanged }: { data: ConfigurationData; sensor: EffectiveSensor; onChanged: () => void }) {
  const row = data.rows!.find((r) => r.enabled && r.name === sensor.name && r.instance === sensor.instance);
  const inherited = data.siteSensors!.find((s) => s.name === sensor.name && s.instance === sensor.instance)?.interval_ms;
  return (
    <IntervalControl
      label="How often to check"
      value={row || inherited === undefined ? sensor.interval_ms : null}
      inherit={inherited === undefined ? undefined : { ms: inherited, label: "site default" }}
      save={async (ms) => {
        await saveCheckInterval({ kind: "servers", id: data.server.id }, sensor, row, ms);
        onChanged();
      }}
    />
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
