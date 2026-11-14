"use client";

import { useMemo, useState } from "react";

import {
  type EffectiveSensor,
  type EffectiveSensors,
  type EffectiveThreshold,
  type SensorConfigEntry,
  type ServerDetail,
  describeError,
} from "@/lib/api";
import { checkTitle, saveThreshold, shapeFor, shapeOf } from "@/lib/alerts";
import { unitOf } from "@/lib/format";
import { modifiesSite, useMe } from "@/lib/me";
import { metricTitle } from "@/lib/metrics";
import { sensorHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { LEVEL_LABELS, perInstance, ruleLevel, ruleSummary, rowRule } from "@/lib/sensorRules";
import { type SensorRow, formatSensorValue, primaryMetric, sensorKind } from "@/lib/sensors";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Button } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, EmptyState } from "@/components/ui/PageState";
import { Reason } from "@/components/ui/Reason";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { type RowMenuItem, RowMenu } from "@/components/ui/RowMenu";
import { Sparkline } from "@/components/ui/Sparkline";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatDuration, formatSince } from "@/components/ui/time";
import type { Tone } from "@/components/ui/tone";

import { AddCheck, BuiltinInterval, type Target, TurnBackOn, stopChecking, stringParams } from "./MonitoringSettings";
import styles from "./ServerPage.module.css";

/** What the Sensors tab reads besides the rows; null where it could not be read. */
export interface SensorRules {
  thresholds: EffectiveThreshold[] | null;
  /** The server's own sensor rows, turned-off ones included. */
  ownRows: SensorConfigEntry[] | null;
  /** What its site runs: the checks it inherits. */
  siteSensors: EffectiveSensor[] | null;
}

/**
 * The Sensors tab: whether the agent runs the current configuration, then
 * one row per sensor with its result and its rule, where the rule comes
 * from, and a menu to edit it, open its chart or turn it off. Operators of
 * the site add checks under the list (ADR-0057).
 */
export function ServerSensorsTab({
  server,
  sensors,
  rows,
  rules,
  window,
  incidentsFailed,
  now,
  onChanged,
}: {
  server: ServerDetail;
  sensors: EffectiveSensors | { error: string };
  rows: SensorRow[];
  rules: SensorRules;
  /** What the sparklines cover; null when the values could not be read. */
  window?: { start: number; end: number } | null;
  incidentsFailed: boolean;
  now: Date;
  onChanged: () => void;
}) {
  const scope = useScope();
  const me = useMe();
  const [adding, setAdding] = useState(false);
  const [confirming, setConfirming] = useState<string | null>(null);
  const { id, site_id: siteId, site_name: siteName, os_family: osFamily } = server;
  const target = useMemo<Target>(() => ({ kind: "server", id, siteId, siteName, osFamily }), [id, siteId, siteName, osFamily]);
  const canEdit = !server.revoked_at && modifiesSite(me, server.customer_id, siteId);

  if ("error" in sensors) {
    return (
      <Banner tone="bad">
        <strong>Could not load the sensor configuration:</strong> {sensors.error} Retrying automatically.
      </Banner>
    );
  }

  const ruleOfRow = (row: SensorRow) =>
    rules.thresholds ? rowRule(sensors.sensors, rules.thresholds, row.sensor, row.metric, row.instance) : undefined;

  function menuItems(row: SensorRow): RowMenuItem[] {
    const rule = ruleOfRow(row);
    // A sensor's rule is edited on the page of the metric it alerts on (disk activity: its wait).
    const ruleMetric = rule?.metric ?? row.metric;
    const items: RowMenuItem[] = [];
    if (canEdit) items.push({ label: "Edit", href: scopedHref(sensorHref(server.id, ruleMetric, row.instance, true), scope) });
    items.push({ label: "Open chart", href: scopedHref(sensorHref(server.id, row.metric, row.instance), scope) });
    // One disk of several mutes its alert, which needs one to mute.
    const muting = perInstance(row.sensor, row.instance);
    if (canEdit && (!muting || rule?.enabled)) {
      const label = !muting && own(row.sensor, rules.siteSensors) ? "Remove…" : "Turn off…";
      items.push({ label, onSelect: () => setConfirming(row.key), danger: true });
    }
    return items;
  }

  const columns: Column<SensorRow>[] = [
    {
      key: "sensor",
      header: "Sensor",
      cell: (row) => (
        <PrimaryCell
          title={row.title}
          href={scopedHref(sensorHref(server.id, row.metric, row.instance), scope)}
          sub={sensorKind(row.sensor, row.instance)}
        />
      ),
    },
    {
      key: "status",
      header: "Status",
      cell: (row) =>
        row.waiting ? <Label tone="neutral">Waiting for first value</Label> : <StatusLabel kind="sensor" value={row.status} />,
    },
    { key: "message", header: "Message", cell: (row) => <Reason short text={row.reason ?? row.error} /> },
    {
      key: "trend",
      header: "Last hour",
      cell: (row) =>
        window ? (
          <Sparkline
            points={row.points}
            start={window.start}
            end={window.end}
            ratio={unitOf(row.metric) === "ratio"}
            tone={TONES[row.status]}
            label={`${row.title}, last hour`}
          />
        ) : null,
    },
    { key: "value", header: "Last value", cell: (row) => <LastValue row={row} now={now} /> },
    {
      key: "rule",
      header: "Rule",
      cell: (row) => (rules.thresholds ? <RuleCell sensors={sensors.sensors} row={row} rule={ruleOfRow(row)} /> : "—"),
    },
  ];

  const addButton = canEdit && !adding && (
    <Button variant="k" onClick={() => setAdding(true)}>
      Add a check
    </Button>
  );
  const off = rules.ownRows?.filter((r) => !r.enabled) ?? [];

  return (
    <div className={styles.tab}>
      <Drift config={sensors} now={now} />
      {incidentsFailed && (
        <Banner tone="warn">Open incidents could not be read: statuses show OK or Unknown until they load.</Banner>
      )}
      {rows.length === 0 ? (
        <EmptyState actions={addButton || undefined}>No sensors: every default is turned off for this server.</EmptyState>
      ) : (
        <DataTable
          label="Sensors"
          columns={columns}
          rows={rows}
          rowKey={(row) => row.key}
          rowActions={(row) => <RowMenu label={`Actions for ${row.title}`} items={menuItems(row)} />}
          rowDetail={(row) =>
            confirming === row.key && (
              <TurnOff
                row={row}
                rule={ruleOfRow(row)}
                target={target}
                hostname={server.hostname}
                siteSensors={rules.siteSensors}
                onClose={() => setConfirming(null)}
                onChanged={onChanged}
              />
            )
          }
        />
      )}
      <div className={styles.addLine}>
        {rows.length > 0 && addButton}
        <BuiltinInterval
          target={target}
          interval={{ check_interval_ms: sensors.check_interval_ms, check_interval_origin: sensors.check_interval_origin }}
          canEdit={canEdit}
          onChanged={onChanged}
        />
      </div>
      {adding && (
        <div className={styles.addBox}>
          <AddCheck
            target={target}
            intervalMs={sensors.check_interval_ms}
            onDone={() => {
              setAdding(false);
              onChanged();
            }}
            onCancel={() => setAdding(false)}
          />
        </div>
      )}
      {off.length > 0 && (
        <section className={styles.offList} aria-label="Turned off on this server">
          <h3 className={styles.offTitle}>Turned off on this server</h3>
          <ul>
            {off.map((row) => (
              <li key={row.id}>
                <span>{checkTitle({ ...row, origin: "server", entry_id: row.id, params: stringParams(row.params) })}</span>
                {canEdit && <TurnBackOn id={row.id} onChanged={onChanged} />}
              </li>
            ))}
          </ul>
        </section>
      )}
      <p className={styles.note}>
        Problems first, then by name. The line shows the last hour. Open a sensor for its chart and history; change
        its rule from its ⋯ menu.
      </p>
    </div>
  );
}

/** A check added on this server, not a copy of one it inherits. */
function own(sensor: EffectiveSensor, siteSensors: EffectiveSensor[] | null): boolean {
  if (sensor.origin !== "server" || !sensor.entry_id) return false;
  return !siteSensors?.some((s) => s.name === sensor.name && s.instance === sensor.instance);
}

/** "every 1 min · warn > 85% · crit > 95% · for 5 min" and where it comes from. */
function RuleCell({ sensors, row, rule }: { sensors: EffectiveSensor[]; row: SensorRow; rule: EffectiveThreshold | undefined }) {
  const shape = rule ? shapeFor(sensors, rule) : shapeOf(sensors, row.metric, row.instance || null);
  const level = ruleLevel(row.sensor, rule, perInstance(row.sensor, row.instance));
  return (
    <span className={styles.rule}>
      <span>{ruleSummary(shape, rule, row.sensor.interval_ms)}</span>
      <Label tone={level === "builtin" || level === "site" ? "neutral" : "info"} small>
        {LEVEL_LABELS[level]}
      </Label>
    </span>
  );
}

/**
 * Turn off, confirmed under the row as before: one disk of several mutes
 * its alert and stays measured; another sensor stops being checked here;
 * a check added on this server is removed with its alert.
 */
function TurnOff({
  row,
  rule,
  target,
  hostname,
  siteSensors,
  onClose,
  onChanged,
}: {
  row: SensorRow;
  rule: EffectiveThreshold | undefined;
  target: Target;
  hostname: string;
  siteSensors: EffectiveSensor[] | null;
  onClose: () => void;
  onChanged: () => void;
}) {
  const common = {
    layout: "row" as const,
    trigger: "Turn off…",
    pendingLabel: "Saving…",
    describeError: (e: unknown) => `It was not changed. ${describeError(e)}`,
    onClose,
  };
  if (perInstance(row.sensor, row.instance) && rule) {
    return (
      <InlineConfirm
        {...common}
        title={`Turn off alerts for ${row.title} on ${hostname}?`}
        consequence="Its values are still recorded. Turn it back on from its page."
        confirmLabel="Turn off"
        onConfirm={async () => {
          const { metric, comparison, warning, critical, duration_s, hysteresis } = rule;
          await saveThreshold(
            { kind: "servers", id: target.id },
            { metric, instance: row.instance, comparison, warning, critical, duration_s, hysteresis, enabled: false },
          );
          onChanged();
        }}
      />
    );
  }
  const removing = own(row.sensor, siteSensors);
  const title = checkTitle(row.sensor);
  return (
    <InlineConfirm
      {...common}
      title={removing ? `Remove ${title} and its alert?` : `Stop checking ${title} on this server?`}
      consequence={removing ? "" : "You can turn it back on here."}
      confirmLabel={removing ? "Remove" : "Turn off"}
      onConfirm={async () => {
        await stopChecking(target, row.sensor, removing);
        onChanged();
      }}
    />
  );
}

const TONES: Record<SensorRow["status"], Tone> = { critical: "bad", warning: "warn", unknown: "neutral", ok: "ok" };

function LastValue({ row, now }: { row: SensorRow; now: Date }) {
  if (row.waiting || row.error) return <span className={styles.muted}>–</span>;
  const { incident } = row;
  const other = incident && incident.metric !== primaryMetric(row.sensor).metric;
  return (
    <>
      <strong>{formatSensorValue(row.metric, row.value)}</strong>
      {incident && (
        <span className={styles.muted}>
          {" · "}
          {other && `${metricTitle(incident.metric)} · `}
          since {formatSince(new Date(incident.opened_at), now)}, {formatDuration((now.getTime() - Date.parse(incident.opened_at)) / 1000)}
        </span>
      )}
    </>
  );
}

/** Whether the agent confirmed the current configuration (ADR-0016). */
function Drift({ config, now }: { config: EffectiveSensors; now: Date }) {
  if (config.applied_revision === null) {
    return (
      <p className={styles.drift}>
        <Label tone="neutral">Waiting for the agent</Label>
        <span>it has not confirmed a configuration yet.</span>
      </p>
    );
  }
  if (config.in_sync) {
    return (
      <p className={styles.drift}>
        <Label tone="ok">Configuration up to date</Label>
        {config.applied_at && (
          <span>
            applied <RelativeTime value={config.applied_at} now={now} />.
          </span>
        )}
      </p>
    );
  }
  return (
    <p className={styles.drift}>
      <Label tone="warn">Configuration out of date</Label>
      <span>a connected agent gets the new one within about 10 seconds.</span>
    </p>
  );
}
