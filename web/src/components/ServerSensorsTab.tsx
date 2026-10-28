import Link from "next/link";

import type { EffectiveSensors, ServerDetail } from "@/lib/api";
import { metricTitle } from "@/lib/metrics";
import { sensorHref, serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { type SensorRow, formatSensorValue, primaryMetric, sensorFacts } from "@/lib/sensors";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Banner, EmptyState } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { formatDuration, formatSince } from "@/components/ui/time";

import styles from "./ServerPage.module.css";

/** The Sensors tab: whether the agent runs the current configuration, then one row per sensor. */
export function ServerSensorsTab({
  server,
  sensors,
  rows,
  incidentsFailed,
  now,
}: {
  server: ServerDetail;
  sensors: EffectiveSensors | { error: string };
  rows: SensorRow[];
  incidentsFailed: boolean;
  now: Date;
}) {
  const scope = useScope();
  if ("error" in sensors) {
    return (
      <Banner tone="bad">
        <strong>Could not load the sensor configuration:</strong> {sensors.error} Retrying automatically.
      </Banner>
    );
  }
  const columns: Column<SensorRow>[] = [
    {
      key: "sensor",
      header: "Sensor",
      cell: (row) => (
        <PrimaryCell
          title={row.title}
          href={scopedHref(sensorHref(server.id, row.metric, row.instance), scope)}
          sub={sensorFacts(row.sensor, row.instance)}
        />
      ),
    },
    { key: "status", header: "Status", cell: (row) => <StatusLabel kind="sensor" value={row.status} /> },
    { key: "value", header: "Last value", cell: (row) => <LastValue row={row} now={now} /> },
  ];
  return (
    <div className={styles.tab}>
      <Drift config={sensors} now={now} />
      {incidentsFailed && (
        <Banner tone="warn">Open incidents could not be read: statuses show OK or Unknown until they load.</Banner>
      )}
      {rows.length === 0 ? (
        <EmptyState>
          No sensors: every default is turned off for this server.{" "}
          <Link href={scopedHref(serverHref(server.id, "alerts"), scope)}>Add a check</Link>.
        </EmptyState>
      ) : (
        <DataTable label="Sensors" columns={columns} rows={rows} rowKey={(row) => row.key} dense />
      )}
      <p className={styles.note}>
        Sorted by status, then name. Open a sensor for its chart and history, or{" "}
        <Link href={scopedHref(serverHref(server.id, "alerts"), scope)}>add a check or change when to alert</Link>.
      </p>
    </div>
  );
}

function LastValue({ row, now }: { row: SensorRow; now: Date }) {
  if (row.error) return <span className={styles.muted}>{row.error}</span>;
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

/** Whether the agent acknowledged the current revision (ADR-0016). */
function Drift({ config, now }: { config: EffectiveSensors; now: Date }) {
  if (config.applied_revision === null) {
    return (
      <p className={styles.drift}>
        <Label tone="neutral">Not applied</Label>
        <span>the agent never acknowledged a configuration; the current revision is {config.revision}.</span>
      </p>
    );
  }
  if (config.in_sync) {
    return (
      <p className={styles.drift}>
        <Label tone="ok">In sync</Label>
        <span>
          the agent runs revision {config.revision}
          {config.applied_at && (
            <>
              , applied <RelativeTime value={config.applied_at} now={now} />
            </>
          )}
          .
        </span>
      </p>
    );
  }
  return (
    <p className={styles.drift}>
      <Label tone="warn">Configuration out of date</Label>
      <span>
        the agent runs revision {config.applied_revision}, the current one is {config.revision}. A connected agent gets it
        within about 10 seconds.
      </span>
    </p>
  );
}
