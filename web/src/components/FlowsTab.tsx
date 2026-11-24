"use client";

import { type FormEvent, useCallback, useState } from "react";

import { t } from "@/i18n";
import {
  type FlowDimension,
  type FlowDimensionView,
  type FlowExporter,
  type FlowView,
  type Probe,
  type ServerDetail,
  ApiError,
  api,
  describeError,
  isUnauthorized,
} from "@/lib/api";
import {
  ADDRESS_METRIC,
  FLOW_DIMENSIONS,
  FLOW_PERIODS,
  type FlowPeriod,
  chartSeries,
  formatBits,
  isAddressDimension,
  isFlowDimension,
  isFlowPeriod,
  keyText,
  parseMbits,
  periodRange,
  share,
} from "@/lib/flows";
import { formatBytes, formatPercent, formatRelative } from "@/lib/format";
import { modifiesSite, useMe } from "@/lib/me";
import { useUrlParams } from "@/lib/urlState";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { EmptyState, ErrorState, LoadingBlock } from "@/components/ui/PageState";
import { SegmentedControl } from "@/components/ui/SegmentedControl";
import { TimeChart } from "@/components/ui/TimeChart";

import styles from "./FlowsTab.module.css";

/** The words of who receives the flows: "the central server", "relay relay-paris". */
function probeWords(exporter: Pick<FlowExporter, "probe" | "relay_name">): string {
  return exporter.probe === "central"
    ? t("flows.probe.central")
    : t("flows.probe.relay", { name: exporter.relay_name ?? exporter.probe });
}

/**
 * The Flows tab of a device (ADR-0184): whether its NetFlow, sFlow or IPFIX
 * is received and by which probe, then the largest addresses, applications,
 * protocols and interfaces over a period, a stacked chart of one of them,
 * and an alert on any address above a rate. Clicking an address filters
 * every view on it.
 */
export function FlowsTab({ server }: { server: ServerDetail }) {
  const me = useMe();
  const canEdit = !server.revoked_at && modifiesSite(me, server.customer_id, server.site_id);
  const fetcher = useCallback(
    async (signal: AbortSignal) => {
      try {
        return await api.deviceFlowExporter(server.id, signal);
      } catch (error) {
        if (error instanceof ApiError && error.status === 404) return null;
        throw error;
      }
    },
    [server.id],
  );
  const { data: exporter, error, refresh } = usePolledData(fetcher);
  const [editing, setEditing] = useState(false);
  if (exporter === undefined) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what={t("flows.load")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("flows.loading")} />
    );
  }
  if (exporter === null || editing) {
    if (!canEdit) return <EmptyState>{t("flows.setup.readOnly", { name: server.hostname })}</EmptyState>;
    return (
      <Setup
        server={server}
        exporter={exporter}
        onDone={() => {
          setEditing(false);
          refresh();
        }}
        onCancel={exporter ? () => setEditing(false) : undefined}
      />
    );
  }
  return (
    <>
      <section className={styles.section} aria-label={t("flows.setup.probe")}>
        <div className={styles.status}>
          <p>
            {t("flows.receivedBy", { probe: probeWords(exporter), address: exporter.address ?? "" })}{" "}
            <span className={styles.dim}>
              {exporter.last_flow_at
                ? t("flows.lastFlow", { when: formatRelative(exporter.last_flow_at, new Date()) })
                : t("flows.noFlowYet", { probe: probeWords(exporter) })}
            </span>
          </p>
          {canEdit && (
            <div className={styles.actions}>
              <Button onClick={() => setEditing(true)}>{t("flows.change")}</Button>
              <InlineConfirm
                layout="row"
                trigger={t("flows.stop")}
                title={t("flows.stop.title", { name: server.hostname })}
                consequence={t("flows.stop.consequence")}
                confirmLabel={t("flows.stop.confirm")}
                pendingLabel={t("flows.stop.pending")}
                describeError={(e) => t("flows.stop.failed", { error: describeError(e) })}
                onConfirm={async () => {
                  await api.deleteDeviceFlowExporter(server.id);
                  refresh();
                }}
              />
            </div>
          )}
        </div>
      </section>
      <FlowViews server={server} />
      {canEdit && <AddressAlert server={server} />}
    </>
  );
}

/** Turns on, or changes, who receives the device's flows. */
function Setup({
  server,
  exporter,
  onDone,
  onCancel,
}: {
  server: ServerDetail;
  exporter: FlowExporter | null;
  onDone: () => void;
  onCancel?: () => void;
}) {
  const fetcher = useCallback((signal: AbortSignal) => api.siteProbes(server.site_id, signal), [server.site_id]);
  const { data } = usePolledData(fetcher, { intervalMs: 60_000 });
  const probes: Probe[] = data?.probes ?? [];
  const value = (p: Probe) => (p.probe === "central" ? "central" : (p.relay_id ?? ""));
  const fallback = probes.find((p) => p.default) ?? probes[0];
  const [probe, setProbe] = useState<string | null>(exporter?.probe ?? null);
  const chosen = probe ?? (fallback ? value(fallback) : "central");
  const [source, setSource] = useState(exporter?.source_address ?? "");
  const [saving, setSaving] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  const address = server.address;
  const chosenProbe = probes.find((p) => value(p) === chosen);
  const words = chosenProbe
    ? probeWords({ probe: chosenProbe.probe === "central" ? "central" : chosen, relay_name: chosenProbe.name })
    : t("flows.probe.central");

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setSaving(true);
    setFailure(null);
    try {
      await api.setDeviceFlowExporter(server.id, { probe: chosen, source_address: source.trim() || null });
      onDone();
    } catch (error) {
      setFailure(t("flows.setup.failed", { error: describeError(error) }));
    } finally {
      setSaving(false);
    }
  };

  return (
    <form className={styles.section} onSubmit={submit} aria-labelledby="flows-setup">
      <h2 id="flows-setup" className={styles.title}>
        {t("flows.setup.title")}
      </h2>
      <p className={styles.note}>{t("flows.setup.note", { name: server.hostname, probe: words })}</p>
      <div className={styles.row}>
        <label className={styles.field}>
          {t("flows.setup.probe")}
          <select className={fieldClass} value={chosen} onChange={(e) => setProbe(e.target.value)}>
            {probes.length === 0 && <option value="central">{t("flows.probe.central")}</option>}
            {probes.map((p) => (
              <option key={value(p)} value={value(p)}>
                {p.probe === "central" ? t("devices.from.central") : t("devices.from.relay", { name: p.name ?? p.key })}
              </option>
            ))}
          </select>
        </label>
        <label className={styles.field}>
          {t("flows.setup.source")}
          <input
            className={fieldClass}
            value={source}
            onChange={(e) => setSource(e.target.value)}
            placeholder={t("flows.setup.sourcePlaceholder")}
            required={!address}
          />
        </label>
        <Button type="submit" variant="k" disabled={saving}>
          {saving ? t("flows.setup.saving") : exporter ? t("flows.setup.save") : t("flows.setup.button")}
        </Button>
        {onCancel && <Button onClick={onCancel}>{t("flows.setup.cancel")}</Button>}
      </div>
      <p className={styles.hint}>
        {address ? t("flows.setup.sourceHint", { address }) : t("flows.setup.sourceNoAddress")}
      </p>
      {failure && (
        <p className={styles.error} role="alert">
          {failure}
        </p>
      )}
    </form>
  );
}

/** The period, the filter, the chart and the tops. */
function FlowViews({ server }: { server: ServerDetail }) {
  const [params, setParams] = useUrlParams();
  const periodParam = params.get("period");
  const period: FlowPeriod = isFlowPeriod(periodParam) ? periodParam : "1h";
  const byParam = params.get("by");
  const by: FlowDimension = isFlowDimension(byParam) ? byParam : "src";
  const address = params.get("address");
  const fetcher = useCallback(
    (signal: AbortSignal) => api.deviceFlows(server.id, { ...periodRange(period, new Date()), by, address }, signal),
    [server.id, period, by, address],
  );
  const { data, error, refresh } = usePolledData(fetcher, { intervalMs: 60_000 });

  return (
    <section className={styles.section} aria-label={t("flows.tab")}>
      <div className={styles.toolbar}>
        <SegmentedControl
          label={t("flows.period")}
          options={FLOW_PERIODS.map((p) => ({ value: p, label: t(`flows.period.${p}`) }))}
          value={period}
          onChange={(p) => setParams({ period: p === "1h" ? null : p })}
        />
        {address && (
          <span className={styles.filter}>
            {t("flows.filter", { address })}
            <Button size="small" onClick={() => setParams({ address: null })}>
              {t("flows.filter.clear")}
            </Button>
          </span>
        )}
      </div>
      {!data ? (
        error !== null && !isUnauthorized(error) ? (
          <ErrorState what={t("flows.load")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock label={t("flows.loading")} />
        )
      ) : data.bytes === 0 ? (
        <EmptyState>{t("flows.empty")}</EmptyState>
      ) : (
        <Views view={data} by={by} filtered={address !== null} onBy={(d) => setParams({ by: d === "src" ? null : d })} onAddress={(a) => setParams({ address: a })} />
      )}
    </section>
  );
}

function Views({
  view,
  by,
  filtered,
  onBy,
  onAddress,
}: {
  view: FlowView;
  by: FlowDimension;
  filtered: boolean;
  onBy: (d: FlowDimension) => void;
  onAddress: (address: string) => void;
}) {
  const start = Math.floor(new Date(view.from).getTime() / 1000);
  const end = Math.floor(new Date(view.to).getTime() / 1000);
  return (
    <>
      <p className={styles.totals}>
        {t("flows.total", { rate: formatBits(view.bits_per_second), bytes: formatBytes(view.bytes) })}
        {!filtered && <> · {t("flows.filter.hint")}</>}
      </p>
      <TimeChart
        title={t("flows.chart.title")}
        actions={
          <select className={fieldClass} value={by} onChange={(e) => onBy(e.target.value as FlowDimension)} aria-label={t("flows.by")}>
            {FLOW_DIMENSIONS.map((d) => (
              <option key={d} value={d}>
                {t(`flows.dimension.${d}`)}
              </option>
            ))}
          </select>
        }
        series={chartSeries(view)}
        start={start}
        end={end}
        step={view.step_s}
        variant="area"
        min={0}
        format={formatBits}
      />
      <div className={styles.grid}>
        {view.dimensions.map((d) => (
          <Top key={d.dimension} dimension={d} total={view.bytes} onAddress={onAddress} />
        ))}
      </div>
    </>
  );
}

function Top({
  dimension,
  total,
  onAddress,
}: {
  dimension: FlowDimensionView;
  total: number;
  onAddress: (address: string) => void;
}) {
  const title = t(`flows.dimension.${dimension.dimension}`);
  const clickable = isAddressDimension(dimension.dimension);
  return (
    <div className={styles.panel}>
      <h3>{title}</h3>
      <table className={styles.table} aria-label={title}>
        <thead>
          <tr>
            <th scope="col">{t("flows.column.key")}</th>
            <th scope="col">{t("flows.column.rate")}</th>
            <th scope="col">{t("flows.column.share")}</th>
          </tr>
        </thead>
        <tbody>
          {dimension.keys.map((k) => {
            const text = keyText(dimension.dimension, k.key, k.label);
            return (
              <tr key={k.key}>
                <td>
                  {clickable ? (
                    <button
                      type="button"
                      className={styles.keyButton}
                      onClick={() => onAddress(k.key)}
                      aria-label={t("flows.filter.button", { address: k.key })}
                    >
                      {text}
                    </button>
                  ) : (
                    text
                  )}
                </td>
                <td>{formatBits(k.bits_per_second)}</td>
                <td>{formatPercent(share(k.bytes, total))}</td>
              </tr>
            );
          })}
          {dimension.other_bytes > 0 && (
            <tr>
              <td className={styles.dim}>{t("flows.other")}</td>
              <td />
              <td className={styles.dim}>{formatPercent(share(dimension.other_bytes, total))}</td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

/** "Alert when an address is above X Mbit/s for N minutes": a threshold of the device (ADR-0153). */
function AddressAlert({ server }: { server: ServerDetail }) {
  const fetcher = useCallback((signal: AbortSignal) => api.thresholdRows({ kind: "servers", id: server.id }, signal), [server.id]);
  const { data, refresh } = usePolledData(fetcher, { intervalMs: 60_000 });
  const existing = data?.entries.find((row) => row.metric === ADDRESS_METRIC);
  const [rate, setRate] = useState("100");
  const [minutes, setMinutes] = useState("5");
  const [state, setState] = useState<{ saving: boolean; message: string | null; failed: boolean }>({
    saving: false,
    message: null,
    failed: false,
  });

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const bits = parseMbits(rate);
    const n = Number(minutes);
    if (bits === null || !Number.isInteger(n) || n < 1 || n > 1440) {
      setState({ saving: false, message: t("flows.alert.invalid"), failed: true });
      return;
    }
    setState({ saving: true, message: null, failed: false });
    try {
      await api.createThreshold(
        { kind: "servers", id: server.id },
        {
          metric: ADDRESS_METRIC,
          instance: null,
          comparison: ">",
          warning: bits,
          critical: null,
          duration_s: n * 60,
          hysteresis: 0,
          enabled: true,
        },
      );
      setState({ saving: false, message: t("flows.alert.done"), failed: false });
      refresh();
    } catch (error) {
      setState({ saving: false, message: t("flows.alert.failed", { error: describeError(error) }), failed: true });
    }
  };

  const limit = existing?.warning ?? existing?.critical ?? null;
  return (
    <form className={styles.section} onSubmit={submit} aria-labelledby="flows-alert">
      <h2 id="flows-alert" className={styles.title}>
        {t("flows.alert.title")}
      </h2>
      <p className={styles.note}>{t("flows.alert.note")}</p>
      {existing && limit !== null ? (
        <p className={styles.hint}>
          {t("flows.alert.existing", { rate: formatBits(limit), minutes: Math.round(existing.duration_s / 60) })}
        </p>
      ) : (
        <div className={styles.row}>
          <label className={styles.field}>
            {t("flows.alert.rate")}
            <input className={`${fieldClass} ${styles.number}`} inputMode="decimal" value={rate} onChange={(e) => setRate(e.target.value)} />
          </label>
          <label className={styles.field}>
            {t("flows.alert.minutes")}
            <input className={`${fieldClass} ${styles.number}`} inputMode="numeric" value={minutes} onChange={(e) => setMinutes(e.target.value)} />
          </label>
          <Button type="submit" disabled={state.saving}>
            {state.saving ? t("flows.alert.saving") : t("flows.alert.button")}
          </Button>
        </div>
      )}
      {state.message && (
        <p className={state.failed ? styles.error : styles.hint} role={state.failed ? "alert" : "status"}>
          {state.message}
        </p>
      )}
    </form>
  );
}
