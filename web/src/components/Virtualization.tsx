"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useState } from "react";

import { t } from "@/i18n";
import { type ServerDetail, type Virtualization, type VirtualObject, api, describeError } from "@/lib/api";
import { formatBytes, formatPercent, formatRelative } from "@/lib/format";
import { administersCustomer, modifiesSite, useMe } from "@/lib/me";
import { serverHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Button } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { missingText } from "@/components/ui/Missing";
import { TableBox } from "@/components/ui/TableBox";
import { formatDuration } from "@/components/ui/time";

import styles from "./AdminPages.module.css";

/** Polled once a minute: sources are read every 5 minutes. */
const REFRESH_MS = 60_000;

const num = (o: VirtualObject, key: string): number | undefined => o.metrics[key];

/** "Running", "Disconnected", "Not seen since 3 h ago". */
export function objectState(o: VirtualObject, now: Date): string {
  if (o.stale) return t("virtualization.state.stale", { when: formatRelative(o.seen_at, now) });
  switch (o.kind) {
    case "vm": {
      const power = o.details.power_state ?? "";
      if (/suspend|saved|paused/i.test(power)) return t("virtualization.state.suspended");
      return num(o, "powered_on") === 1 ? t("virtualization.state.on") : t("virtualization.state.off");
    }
    case "host": {
      if (num(o, "maintenance") === 1) return t("virtualization.state.maintenance");
      const state = o.details.connection_state;
      if (state === "notResponding") return t("virtualization.state.notResponding");
      return num(o, "connected") === 1 ? t("virtualization.state.connected") : t("virtualization.state.disconnected");
    }
    case "datastore":
      return num(o, "accessible") === 1 ? t("virtualization.state.available") : t("virtualization.state.inaccessible");
    default: {
      const status = num(o, "status");
      return status === 1 || status === 2 ? t(`virtualization.status.${status}`) : t("virtualization.state.available");
    }
  }
}

/** "2 vCPU, 12 % used". */
function cpu(o: VirtualObject): string {
  const count = num(o, "cpu_count") ?? num(o, "cpu_cores");
  const use = num(o, "cpu_usage_ratio");
  if (count === undefined) return use === undefined ? missingText("noData") : formatPercent(use);
  return use === undefined
    ? t("virtualization.cpu.cores", { count: String(count) })
    : t("virtualization.cpu.use", { count: String(count), use: formatPercent(use) });
}

/** "4 GiB, 40 % used". */
function memory(o: VirtualObject): string {
  const bytes = num(o, "memory_bytes");
  const use = num(o, "memory_usage_ratio");
  if (bytes === undefined) return missingText("noData");
  return use === undefined ? formatBytes(bytes) : `${formatBytes(bytes)}, ${formatPercent(use)}`;
}

/** "None", "2, oldest 9 d". */
export function snapshots(o: VirtualObject): string {
  const count = num(o, "snapshots") ?? 0;
  const age = num(o, "snapshot_age_seconds");
  if (count === 0) return t("virtualization.snapshots.none");
  return t("virtualization.snapshots.count", { count: String(count), age: age === undefined ? "" : formatDuration(age) });
}

function alarms(o: VirtualObject): string | null {
  const critical = num(o, "alarms_critical") ?? 0;
  const warning = num(o, "alarms_warning") ?? 0;
  return critical + warning === 0 ? null : t("virtualization.alarms.count", { critical: String(critical), warning: String(warning) });
}

/** How a virtual machine was tied to its server, in words. */
export function matchedBy(o: VirtualObject): string {
  return o.matched_by ? t(`virtualization.match.${o.matched_by}`) : "";
}

/** What a server page shows: its virtual machine, a source's inventory, or "Watch Hyper-V". */
export function VirtualizationSection({ server, hypervWatched = false }: { server: ServerDetail; hypervWatched?: boolean }) {
  const fetcher = useCallback((signal: AbortSignal) => api.serverVirtualization(server.id, signal), [server.id]);
  const { data, error, updatedAt, refresh } = usePolledData(fetcher, { intervalMs: REFRESH_MS });
  const now = updatedAt ?? new Date();
  // Most servers have nothing to show here: a failed read shows nothing
  // either, unless the page had shown an inventory already.
  if (!data) return null;
  void error;
  const source = data.source || data.objects.length > 0;
  return (
    <>
      {data.guest_of.map((g) => (
        <GuestLine key={g.object.id} guest={g} />
      ))}
      {source && <Inventory server={server} data={data} now={now} onChanged={refresh} />}
      {!source && hypervWatched && <p className={styles.sectionNote}>{t("virtualization.source.never")}</p>}
      {!source && !hypervWatched && server.kind === "agent" && server.os_family === "windows" && !server.revoked_at && (
        <WatchHyperv server={server} onWatched={refresh} />
      )}
    </>
  );
}

function GuestLine({ guest }: { guest: Virtualization["guest_of"][number] }) {
  const scope = useScope();
  const o = guest.object;
  const values = {
    name: o.name,
    host: o.details.host ?? "",
    source: guest.source_name,
    how: matchedBy(o),
  };
  return (
    <p className={styles.sectionNote} role="note">
      {o.details.host ? t("virtualization.guest.line", values) : t("virtualization.guest.lineNoHost", values)}{" "}
      {t("virtualization.guest.snapshots", { snapshots: snapshots(o) })}{" "}
      <Link href={scopedHref(serverHref(guest.source_id), scope)}>{guest.source_name}</Link>
    </p>
  );
}

function SourceLine({ data, now, server }: { data: Virtualization; now: Date; server: ServerDetail }) {
  const source = data.source;
  if (!source) return <p className={styles.sectionNote}>{t("virtualization.source.never")}</p>;
  const when = formatRelative(source.seen_at, now);
  if (source.up) {
    const product = source.details.product;
    return (
      <p className={styles.sectionNote} role="status">
        {product ? t("virtualization.source.ok", { when, product }) : t("virtualization.source.okNoProduct", { when })}
      </p>
    );
  }
  return (
    <p className={styles.formError} role="status">
      {t("virtualization.source.down", { when, error: source.error ?? source.reason ?? "" })}
      {source.reason === "certificate" && server.kind === "device" && (
        <>
          {" "}
          {t("virtualization.source.certificate")}
        </>
      )}
    </p>
  );
}

function Inventory({
  server,
  data,
  now,
  onChanged,
}: {
  server: ServerDetail;
  data: Virtualization;
  now: Date;
  onChanged: () => void;
}) {
  const hyperv = server.kind === "agent";
  const of = (kind: VirtualObject["kind"]) => data.objects.filter((o) => o.kind === kind);
  return (
    <section aria-label={t("virtualization.title")}>
      <SourceLine data={data} now={now} server={server} />
      {of("host").length > 0 && <HostTable rows={of("host")} now={now} />}
      {of("cluster").length > 0 && <ClusterTable rows={of("cluster")} now={now} />}
      <VmTable server={server} rows={of("vm")} now={now} hyperv={hyperv} onChanged={onChanged} />
      {of("datastore").length > 0 && <DatastoreTable rows={of("datastore")} now={now} />}
    </section>
  );
}

const nameColumn = (now: Date): Column<VirtualObject> => ({
  key: "name",
  header: t("virtualization.col.name"),
  cell: (o) => <PrimaryCell title={o.name} sub={objectState(o, now)} />,
  sortValue: (o) => o.name,
});

const alarmColumn: Column<VirtualObject> = {
  key: "alarms",
  header: t("virtualization.col.alarms"),
  cell: (o) => alarms(o) ?? t("virtualization.alarms.none"),
  sortValue: (o) => num(o, "alarms_critical") ?? 0,
};

function HostTable({ rows, now }: { rows: VirtualObject[]; now: Date }) {
  const columns: Column<VirtualObject>[] = [
    nameColumn(now),
    { key: "cluster", header: t("virtualization.col.cluster"), cell: (o) => o.details.cluster ?? missingText("none") },
    { key: "cpu", header: t("virtualization.col.cpu"), cell: cpu, sortValue: (o) => num(o, "cpu_usage_ratio") ?? null },
    { key: "memory", header: t("virtualization.col.memory"), cell: memory, sortValue: (o) => num(o, "memory_usage_ratio") ?? null },
    alarmColumn,
  ];
  return (
    <TableBox title={t("virtualization.kind.host")}>
      <DataTable label={t("virtualization.kind.host")} columns={columns} rows={rows} rowKey={(o) => o.id} rowMuted={(o) => o.stale} dense bare />
    </TableBox>
  );
}

function ClusterTable({ rows, now }: { rows: VirtualObject[]; now: Date }) {
  const columns: Column<VirtualObject>[] = [
    nameColumn(now),
    {
      key: "hosts",
      header: t("virtualization.col.hosts"),
      cell: (o) =>
        t("virtualization.hosts.count", {
          effective: String(num(o, "hosts_effective") ?? 0),
          total: String(num(o, "hosts") ?? 0),
        }),
    },
    alarmColumn,
  ];
  return (
    <TableBox title={t("virtualization.kind.cluster")}>
      <DataTable label={t("virtualization.kind.cluster")} columns={columns} rows={rows} rowKey={(o) => o.id} dense bare />
    </TableBox>
  );
}

function DatastoreTable({ rows, now }: { rows: VirtualObject[]; now: Date }) {
  const columns: Column<VirtualObject>[] = [
    nameColumn(now),
    {
      key: "free",
      header: t("virtualization.col.free"),
      cell: (o) => {
        const free = num(o, "free_bytes");
        const ratio = num(o, "free_ratio");
        if (free === undefined) return missingText("noData");
        return ratio === undefined ? formatBytes(free) : t("virtualization.free.ratio", { free: formatBytes(free), ratio: formatPercent(ratio) });
      },
      sortValue: (o) => num(o, "free_ratio") ?? null,
    },
    {
      key: "capacity",
      header: t("virtualization.col.capacity"),
      cell: (o) => {
        const capacity = num(o, "capacity_bytes");
        return capacity === undefined ? missingText("noData") : formatBytes(capacity);
      },
    },
    alarmColumn,
  ];
  return (
    <TableBox title={t("virtualization.kind.datastore")}>
      <DataTable label={t("virtualization.kind.datastore")} columns={columns} rows={rows} rowKey={(o) => o.id} rowMuted={(o) => o.stale} dense bare />
    </TableBox>
  );
}

function VmTable({
  server,
  rows,
  now,
  hyperv,
  onChanged,
}: {
  server: ServerDetail;
  rows: VirtualObject[];
  now: Date;
  hyperv: boolean;
  onChanged: () => void;
}) {
  const me = useMe();
  const scope = useScope();
  const router = useRouter();
  const [busy, setBusy] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const canModify = modifiesSite(me, server.customer_id, server.site_id);
  const canInstall = administersCustomer(me, server.customer_id);

  async function act(o: VirtualObject, action: () => Promise<unknown>, failed: "match.failed" | "install.failed") {
    setBusy(o.id);
    setProblem(null);
    try {
      const result = await action();
      if (failed === "install.failed" && result && typeof result === "object" && "id" in result) {
        router.push(scopedHref(serverHref(String(result.id)), scope));
        return;
      }
      onChanged();
    } catch (e) {
      setProblem(t(`virtualization.${failed}`, { error: describeError(e) }));
    } finally {
      setBusy(null);
    }
  }

  const columns: Column<VirtualObject>[] = [
    nameColumn(now),
    ...(hyperv ? [] : [{ key: "host", header: t("virtualization.col.host"), cell: (o: VirtualObject) => o.details.host ?? missingText("unknown") }]),
    { key: "cpu", header: t("virtualization.col.cpu"), cell: cpu, sortValue: (o) => num(o, "cpu_usage_ratio") ?? null },
    { key: "memory", header: t("virtualization.col.memory"), cell: memory },
    {
      key: "snapshots",
      header: hyperv ? t("virtualization.col.checkpoints") : t("virtualization.col.snapshots"),
      cell: (o) => {
        const health = num(o, "replication_health");
        const replication =
          health === undefined ? "" : ` · ${t("virtualization.col.replication")}: ${t(`virtualization.replication.${health as 0}`)}`;
        return snapshots(o) + replication;
      },
      sortValue: (o) => num(o, "snapshot_age_seconds") ?? null,
    },
    {
      key: "server",
      header: t("virtualization.col.server"),
      cell: (o) =>
        o.server ? (
          <PrimaryCell
            title={o.server.hostname}
            href={scopedHref(serverHref(o.server.id), scope)}
            sub={o.server.awaiting_agent ? t("virtualization.server.waiting") : matchedBy(o)}
          />
        ) : (
          t("virtualization.server.none")
        ),
      sortValue: (o) => o.server?.hostname ?? null,
    },
  ];
  return (
    <TableBox title={t("virtualization.kind.vm")}>
      {problem && (
        <p className={styles.formError} role="alert">
          {problem}
        </p>
      )}
      {rows.length === 0 ? (
        <p className={styles.sectionNote}>{t("virtualization.empty")}</p>
      ) : (
        <DataTable
          label={t("virtualization.kind.vm")}
          columns={columns}
          rows={rows}
          rowKey={(o) => o.id}
          rowMuted={(o) => o.stale}
          dense
          bare
          rowActions={(o) => (
            <>
              {!o.server && canInstall && (
                <Button
                  size="small"
                  disabled={busy !== null}
                  onClick={() => void act(o, () => api.installAgentOnVm(o.id), "install.failed")}
                >
                  {busy === o.id ? t("virtualization.install.busy") : t("virtualization.install.button")}
                </Button>
              )}
              {o.server && !o.link_fixed && canModify && (
                <Button
                  variant="ghost"
                  size="small"
                  disabled={busy !== null}
                  onClick={() => void act(o, () => api.linkVirtualObject(o.id, { server_id: null }), "match.failed")}
                >
                  {t("virtualization.match.notThis")}
                </Button>
              )}
              {o.link_fixed && canModify && (
                <Button
                  variant="ghost"
                  size="small"
                  disabled={busy !== null}
                  onClick={() => void act(o, () => api.linkVirtualObject(o.id, { automatic: true }), "match.failed")}
                >
                  {t("virtualization.match.automatic")}
                </Button>
              )}
            </>
          )}
        />
      )}
    </TableBox>
  );
}

/** The command that lets the agent read Hyper-V. */
export const HYPERV_GROUP_COMMAND = 'Add-LocalGroupMember -Group "Hyper-V Administrators" -Member "NT SERVICE\\MonitorFlowAgent"';

function WatchHyperv({ server, onWatched }: { server: ServerDetail; onWatched: () => void }) {
  const me = useMe();
  const [busy, setBusy] = useState(false);
  const [done, setDone] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  if (!modifiesSite(me, server.customer_id, server.site_id)) return null;

  async function watch() {
    setBusy(true);
    setProblem(null);
    try {
      await api.watchHyperv(server.id);
      setDone(true);
      onWatched();
    } catch (e) {
      setProblem(t("virtualization.hyperv.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  if (done) return <p className={styles.sectionNote} role="status">{t("virtualization.hyperv.added")}</p>;
  return (
    <details className={styles.panel}>
      <summary>{t("virtualization.hyperv.watch")}</summary>
      <p className={styles.sectionNote}>{t("virtualization.hyperv.note")}</p>
      <pre>
        <code>{HYPERV_GROUP_COMMAND}</code>
      </pre>
      <p className={styles.sectionNote}>{t("virtualization.hyperv.restart")}</p>
      {problem && (
        <p className={styles.formError} role="alert">
          {problem}
        </p>
      )}
      <div className={styles.formFoot}>
        <Button onClick={() => void watch()} disabled={busy}>
          {busy ? t("virtualization.hyperv.watching") : t("virtualization.hyperv.watch")}
        </Button>
      </div>
    </details>
  );
}
