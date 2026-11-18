"use client";

import { useMemo, useState } from "react";

import { t } from "@/i18n";
import {
  type EffectiveSensor,
  type EffectiveSensors,
  type EffectiveThreshold,
  type SensorConfigEntry,
  type ServerDetail,
  type Silence,
  type SilencedCheck,
  describeError,
} from "@/lib/api";
import { checkTitle, saveThreshold, shapeFor, shapeOf } from "@/lib/alerts";
import { unitOf } from "@/lib/format";
import { coversCheck } from "@/lib/maintenance";
import { inOneHour, tomorrowMorning } from "@/lib/silences";
import { formatDay } from "@/components/ui/time";
import { modifiesSite, useMe } from "@/lib/me";
import { sensorHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { LEVEL_LABELS, perInstance, ruleCompact, ruleLevel, ruleSummary, rowRule } from "@/lib/sensorRules";
import { type SensorRow, formatSensorValue } from "@/lib/sensors";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Button } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Banner, EmptyState } from "@/components/ui/PageState";
import { Reason } from "@/components/ui/Reason";
import { Tooltip } from "@/components/ui/Hint";
import { type RowMenuItem, RowMenu } from "@/components/ui/RowMenu";
import { Sparkline } from "@/components/ui/Sparkline";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import type { Tone } from "@/components/ui/tone";

import { AddCheck, type Target, TurnBackOn, stopChecking, stringParams } from "./MonitoringSettings";
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
  onChanged,
  checkSilences = [],
  onSilenceCheck,
}: {
  server: ServerDetail;
  sensors: EffectiveSensors | { error: string };
  rows: SensorRow[];
  rules: SensorRules;
  /** What the sparklines cover; null when the values could not be read. */
  window?: { start: number; end: number } | null;
  incidentsFailed: boolean;
  onChanged: () => void;
  /** Active silences of chosen checks of this server or its site (ADR-0151). */
  checkSilences?: Silence[];
  /** Silences one check until a time; operators of the site. */
  onSilenceCheck?: (check: SilencedCheck, end: Date) => Promise<void>;
}) {
  const scope = useScope();
  const me = useMe();
  const [adding, setAdding] = useState(false);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [silenceError, setSilenceError] = useState<string | null>(null);
  const { id, site_id: siteId, site_name: siteName, os_family: osFamily } = server;
  const target = useMemo<Target>(() => ({ kind: "server", id, siteId, siteName, osFamily }), [id, siteId, siteName, osFamily]);
  const canEdit = !server.revoked_at && modifiesSite(me, server.customer_id, siteId);

  if ("error" in sensors) {
    return (
      <Banner tone="bad">
        <strong>{t("server.sensors.loadFailed")}</strong> {t("server.sensors.retrying", { error: sensors.error })}
      </Banner>
    );
  }

  const ruleOfRow = (row: SensorRow) =>
    rules.thresholds ? rowRule(sensors.sensors, rules.thresholds, row.sensor, row.metric, row.instance) : undefined;

  /** The silence of chosen checks covering `row` that ends last, if any. */
  const silenceOf = (row: SensorRow) =>
    checkSilences
      .filter((s) => coversCheck(s.checks ?? [], row.sensor.name, row.sensor.instance))
      .reduce<Silence | undefined>((a, s) => (a && a.ends_at > s.ends_at ? a : s), undefined);

  function silenceRow(row: SensorRow, end: Date) {
    setSilenceError(null);
    onSilenceCheck?.({ name: row.sensor.name, instance: row.sensor.instance }, end).catch((e) => setSilenceError(describeError(e)));
  }

  function menuItems(row: SensorRow): RowMenuItem[] {
    const rule = ruleOfRow(row);
    // A sensor's rule is edited on the page of the metric it alerts on (disk activity: its wait).
    const ruleMetric = rule?.metric ?? row.metric;
    const items: RowMenuItem[] = [];
    if (canEdit) items.push({ label: t("server.sensors.edit"), href: scopedHref(sensorHref(server.id, ruleMetric, row.instance, true), scope) });
    items.push({ label: t("server.sensors.openChart"), href: scopedHref(sensorHref(server.id, row.metric, row.instance), scope) });
    if (canEdit && onSilenceCheck) {
      items.push({ label: t("maintenance.check.hour"), onSelect: () => silenceRow(row, inOneHour(new Date())) });
      items.push({ label: t("maintenance.check.tomorrow"), onSelect: () => silenceRow(row, tomorrowMorning(new Date())) });
    }
    // One disk of several mutes its alert, which needs one to mute.
    const muting = perInstance(row.sensor, row.instance);
    if (canEdit && (!muting || rule?.enabled)) {
      const label = !muting && own(row.sensor, rules.siteSensors) ? t("server.sensors.remove") : t("server.sensors.turnOff");
      items.push({ label, onSelect: () => setConfirming(row.key), danger: true });
    }
    return items;
  }

  const columns: Column<SensorRow>[] = [
    {
      key: "sensor",
      header: t("server.sensors.column.sensor"),
      cell: (row) => (
        <PrimaryCell
          title={row.title}
          href={scopedHref(sensorHref(server.id, row.metric, row.instance), scope)}
          // An incident's reason repeats the status, the value and the rule beside it, and
          // the sensor's kind ("disk · /") its title: only why a sensor reports nothing.
          sub={
            !row.incident && row.status !== "ok" && (row.reason ?? row.error) ? (
              <span className={styles.why}>
                <Reason short text={row.reason ?? row.error} />
              </span>
            ) : undefined
          }
        />
      ),
    },
    {
      key: "status",
      header: t("server.sensors.column.status"),
      cell: (row) => {
        const silence = silenceOf(row);
        return (
          <>
            {row.waiting ? <Label tone="neutral">{t("server.sensors.waiting")}</Label> : <StatusLabel kind="sensor" value={row.status} />}
            {silence && (
              <>
                {" "}
                <Label tone="neutral">
                  {t(silence.window_id ? "maintenance.check.inMaintenance" : "maintenance.check.silenced", {
                    when: formatDay(new Date(silence.ends_at), new Date()),
                  })}
                </Label>
              </>
            )}
          </>
        );
      },
    },
    {
      key: "trend",
      header: t("server.sensors.column.lastHour"),
      cell: (row) =>
        window ? (
          <Sparkline
            points={row.points}
            start={window.start}
            end={window.end}
            ratio={unitOf(row.metric) === "ratio"}
            tone={TONES[row.status]}
            label={t("server.sensors.sparkline", { title: row.title })}
          />
        ) : null,
    },
    { key: "value", header: t("server.sensors.column.lastValue"), cell: (row) => <LastValue row={row} /> },
    {
      key: "rule",
      header: t("server.sensors.column.rule"),
      cell: (row) =>
        rules.thresholds ? (
          <RuleCell sensors={sensors.sensors} row={row} rule={ruleOfRow(row)} device={server.kind === "device"} />
        ) : null,
    },
  ];

  const addButton = canEdit && !adding && (
    <Button variant="k" onClick={() => setAdding(true)}>
      {t("server.sensors.add")}
    </Button>
  );
  const off = rules.ownRows?.filter((r) => !r.enabled) ?? [];

  return (
    <div className={styles.tab}>
      {/* A device has no agent to confirm a configuration (ADR-0148). */}
      {server.kind !== "device" && <Drift config={sensors} />}
      {incidentsFailed && (
        <Banner tone="warn">{t("server.sensors.incidentsFailed")}</Banner>
      )}
      {silenceError && (
        <Banner tone="bad">
          <span role="alert">{silenceError}</span>
        </Banner>
      )}
      {rows.length === 0 ? (
        <EmptyState actions={addButton || undefined}>{t("server.sensors.none")}</EmptyState>
      ) : (
        <DataTable
          label={t("server.sensors.table")}
          columns={columns}
          rows={rows}
          rowKey={(row) => row.key}
          rowActions={(row) => <RowMenu label={t("server.sensors.actions", { title: row.title })} items={menuItems(row)} />}
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
      {rows.length > 0 && addButton && <div className={styles.addLine}>{addButton}</div>}
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
        <section className={styles.offList} aria-label={t("server.sensors.offList")}>
          <h3 className={styles.offTitle}>{t("server.sensors.offList")}</h3>
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
    </div>
  );
}

/** A check added on this server, not a copy of one it inherits. */
function own(sensor: EffectiveSensor, siteSensors: EffectiveSensor[] | null): boolean {
  if (sensor.origin !== "server" || !sensor.entry_id) return false;
  return !siteSensors?.some((s) => s.name === sensor.name && s.instance === sensor.instance);
}

/** "1 min · > 85% / 95% · 5 min" and where it comes from; the sentence in a tooltip. */
function RuleCell({
  sensors,
  row,
  rule,
  device,
}: {
  sensors: EffectiveSensor[];
  row: SensorRow;
  rule: EffectiveThreshold | undefined;
  device: boolean;
}) {
  const shape = rule ? shapeFor(sensors, rule) : shapeOf(sensors, row.metric, row.instance || null);
  const level = ruleLevel(row.sensor, rule, perInstance(row.sensor, row.instance));
  return (
    <span className={styles.rule}>
      <Tooltip text={ruleSummary(shape, rule, row.sensor.interval_ms)}>{ruleCompact(shape, rule, row.sensor.interval_ms)}</Tooltip>
      <Label tone="neutral" small>
        {device && level === "server" ? t("devices.level") : LEVEL_LABELS[level]}
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
    trigger: t("server.sensors.turnOff"),
    pendingLabel: t("server.sensors.saving"),
    describeError: (e: unknown) => t("server.sensors.notChanged", { error: describeError(e) }),
    onClose,
  };
  if (perInstance(row.sensor, row.instance) && rule) {
    return (
      <InlineConfirm
        {...common}
        title={t("server.sensors.offAlertsTitle", { title: row.title, hostname })}
        consequence={t("server.sensors.offAlertsConsequence")}
        confirmLabel={t("server.sensors.turnOffConfirm")}
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
      title={removing ? t("server.sensors.removeTitle", { title }) : t("server.sensors.stopTitle", { title })}
      consequence={removing ? "" : t("server.sensors.stopConsequence")}
      confirmLabel={removing ? t("server.sensors.removeConfirm") : t("server.sensors.turnOffConfirm")}
      onConfirm={async () => {
        await stopChecking(target, row.sensor, removing);
        onChanged();
      }}
    />
  );
}

const TONES: Record<SensorRow["status"], Tone> = { critical: "bad", warning: "warn", unknown: "neutral", ok: "ok" };

function LastValue({ row }: { row: SensorRow }) {
  if (row.waiting || row.error) return <span className={styles.muted}>–</span>;
  return <span className={styles.value}>{formatSensorValue(row.metric, row.value)}</span>;
}

/** Whether the agent confirmed the current configuration (ADR-0016). */
function Drift({ config }: { config: EffectiveSensors }) {
  if (config.applied_revision === null) {
    return (
      <p className={styles.drift}>
        <Label tone="neutral">{t("server.sensors.waitingAgent")}</Label>
        <span>{t("server.sensors.notConfirmed")}</span>
      </p>
    );
  }
  // Up to date is the normal state: nothing to say.
  if (config.in_sync) return null;
  return (
    <p className={styles.drift}>
      <Label tone="warn">{t("server.sensors.outOfDate")}</Label>
      <span>{t("server.sensors.soon")}</span>
    </p>
  );
}
