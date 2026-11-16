"use client";

import { type ReactNode, useCallback } from "react";

import { t } from "@/i18n";
import {
  type Package,
  type ServerDetail,
  type ThirdPartyManager,
  type ThirdPartyPackage,
  api,
  describeError,
} from "@/lib/api";
import { MANAGER_NAMES, THIRD_PARTY_MANAGERS, managerFits } from "@/lib/campaigns";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./PendingUpdates.module.css";
import { type Column, DataTable, PrimaryCell } from "./ui/DataTable";
import { ErrorState, LoadingBlock } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";
import { TableBox } from "./ui/TableBox";

/** One row of a source's table: what is updated, from which version to which. */
interface Row {
  key: string;
  name: string;
  sub: string;
  from: string;
  to: string;
}

const columns = (): Column<Row>[] => [
  {
    key: "name",
    header: t("server.pending.column.name"),
    sortValue: (r) => r.name,
    cell: (r) => <PrimaryCell title={r.name} sub={r.sub} />,
  },
  {
    key: "version",
    header: t("server.pending.column.version"),
    sortValue: (r) => r.to,
    cell: (r) => `${r.from} → ${r.to}`,
  },
];

const osRow = (p: Package): Row => ({
  key: `${p.name}:${p.arch}`,
  name: p.name,
  sub: p.title ?? p.arch,
  from: p.version,
  to: p.upgradable_version ?? "",
});

const appRow = (p: ThirdPartyPackage): Row => ({
  key: `${p.id}:${p.scope}`,
  name: p.name ?? p.id,
  sub: [p.name ? p.id : null, p.origin].filter(Boolean).join(" · "),
  from: p.version,
  to: p.available_version ?? "",
});

/** One source: its name, its count or state, and its pending updates. */
function Source({ name, status, rows }: { name: string; status: ReactNode; rows: Row[] }) {
  return (
    <section className={styles.source} aria-label={name}>
      <h3 className={styles.head}>
        {name} {status}
      </h3>
      {rows.length > 0 && (
        <DataTable dense bare label={name} columns={columns()} rows={rows} rowKey={(r) => r.key} pageSize={10} />
      )}
    </section>
  );
}

const count = (n: number) =>
  n === 0 ? <Label tone="ok">{t("server.pending.upToDate")}</Label> : <Label tone="warn">{t("server.pending.count", { count: n })}</Label>;

/** A third-party manager's line: its pending count, or why there is none. */
function managerStatus(manager: ThirdPartyManager | undefined): ReactNode {
  if (!manager) return <span className={styles.dim}>{t("server.pending.notReported")}</span>;
  if (manager.state === "absent") return <span className={styles.dim}>{t("server.pending.absent")}</span>;
  if (manager.state === "unusable") {
    return (
      <>
        <Label tone="bad">{t("server.pending.unusable")}</Label>
        {manager.error && <span className={styles.dim}> {manager.error}</span>}
      </>
    );
  }
  return count(manager.pending);
}

/**
 * Pending updates of a server, grouped by source (ADR-0138): the operating
 * system first, then each third-party manager of its platform.
 */
export function PendingUpdates({ server }: { server: ServerDetail }) {
  const osFetcher = useCallback((signal: AbortSignal) => api.serverPackages(server.id, signal), [server.id]);
  const appFetcher = useCallback((signal: AbortSignal) => api.serverThirdParty(server.id, signal), [server.id]);
  const os = usePolledData(osFetcher);
  const apps = usePolledData(appFetcher);

  if (!os.data || !apps.data) {
    const error = os.error ?? apps.error;
    return error ? (
      <ErrorState
        what={t("server.pending.what")}
        error={describeError(error)}
        onRetry={() => {
          os.refresh();
          apps.refresh();
        }}
      />
    ) : (
      <LoadingBlock label={t("server.pending.loading")} />
    );
  }

  const osPending = os.data.packages.filter((p) => p.upgradable_version !== null);
  const managers = THIRD_PARTY_MANAGERS.filter((m) => managerFits(m, server.os_family));
  return (
    <TableBox title={t("server.pending.title")}>
      <div className={styles.sources}>
        <Source
          name={t("server.pending.os")}
          status={os.data.reported_at === null ? <span className={styles.dim}>{t("server.pending.notReported")}</span> : count(osPending.length)}
          rows={osPending.map(osRow)}
        />
        {managers.map((manager) => {
          const status = apps.data?.managers.find((m) => m.manager === manager);
          const rows = (apps.data?.packages ?? [])
            .filter((p) => p.manager === manager && p.available_version !== null)
            .map(appRow);
          return <Source key={manager} name={MANAGER_NAMES[manager]} status={managerStatus(status)} rows={rows} />;
        })}
        {!apps.data.reported && <p className={styles.note}>{t("server.pending.appsUnknown")}</p>}
      </div>
    </TableBox>
  );
}
