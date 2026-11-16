"use client";

import { useCallback } from "react";

import { t } from "@/i18n";
import { type Rollback, type ServerDetail, api, describeError } from "@/lib/api";
import { localReasonText, snapshotReason } from "@/lib/remediation";
import { type SnapshotSet, covers, listWords, rollbackSteps, snapshotSpace } from "@/lib/rollback";
import { usePolledData } from "@/lib/usePolledData";

import admin from "./AdminPages.module.css";
import styles from "./ServerRollback.module.css";
import { ProxmoxRollback, useProxmox } from "./ProxmoxRollback";
import { type Column, DataTable } from "./ui/DataTable";
import { Hint } from "./ui/Hint";
import { ErrorState, LoadingBlock } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { Label } from "./ui/StatusLabel";
import { formatExact } from "./ui/time";

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** What a server's rollback is, polled for everyone who sees the server. */
export function useRollback(serverId: string, enabled = true) {
  const fetcher = useCallback((signal: AbortSignal) => api.serverRollback(serverId, signal), [serverId]);
  return usePolledData(fetcher, { enabled });
}

/**
 * The one Rollback line of the server page (ADR-0093, ADR-0094), in the
 * order changes follow: a Proxmox guest's line, with its mapping for
 * administrators of the customer; else the local LVM or ZFS snapshot; else
 * why rollback is limited.
 */
export function RollbackFact({ server, canMap }: { server: ServerDetail; canMap: boolean }) {
  const rollback = useRollback(server.id);
  const proxmox = useProxmox(server.id, canMap);
  const proxmoxView = proxmox.data && proxmox.data.clusters.length > 0 ? proxmox.data : null;
  const data = rollback.data;
  if (!data) return <>{rollback.error ? t("serverUpdate.rollback.unknown") : "…"}</>;
  const local = data.snapshots.method !== "proxmox" && !proxmoxGuest(data) ? <LocalLine data={data} /> : null;
  if (proxmoxView) {
    const refresh = () => {
      proxmox.refresh();
      rollback.refresh();
    };
    return <ProxmoxRollback view={proxmoxView} onChanged={refresh} local={local} />;
  }
  return local ?? <GuestLine data={data} />;
}

/** "Rollback limited ⓘ" when changes would run without a snapshot; nothing otherwise. */
export function RollbackBadge({ serverId }: { serverId: string }) {
  const { data } = useRollback(serverId);
  if (!data || data.snapshots.available) return null;
  const { snapshots } = data;
  const why =
    snapshots.method !== "proxmox" && !proxmoxGuest(data)
      ? t("serverUpdate.rollback.withoutSnapshot", { reason: capitalize(localReasonText(snapshots) ?? t("serverUpdate.rollback.noSupport")) })
      : capitalize(snapshotReason(snapshots.reason, snapshots.kind));
  return (
    <Hint tone="warn" label={t("serverUpdate.rollback.limited")}>
      {why}
    </Hint>
  );
}

/** The server is a Proxmox guest whose snapshot Proxmox decides. */
function proxmoxGuest(data: Rollback): boolean {
  const { reason } = data.snapshots;
  return reason === "cannot_snapshot" || reason === "unknown_cluster";
}

/** A Proxmox guest, for those who cannot change its mapping. */
function GuestLine({ data }: { data: Rollback }) {
  const { snapshots } = data;
  if (snapshots.available) {
    return (
      <>
        <Label tone="ok">{t("serverUpdate.rollback.snapshotFirst")}</Label>{" "}
        {snapshots.kind === "lxc" ? t("serverUpdate.rollback.guestContainer") : t("serverUpdate.rollback.guestVm")}
      </>
    );
  }
  return (
    <>
      <Label tone="warn">{t("serverUpdate.rollback.limited")}</Label> {capitalize(snapshotReason(snapshots.reason, snapshots.kind))}
      {snapshots.detail && <span className={styles.note}> {capitalize(snapshots.detail)}.</span>}
    </>
  );
}

/** A server that is no Proxmox guest: its local snapshot, or why there is none. */
function LocalLine({ data }: { data: Rollback }) {
  const { snapshots, not_covered: notCovered } = data;
  if (!snapshots.available) {
    return (
      <>
        <Label tone="warn">{t("serverUpdate.rollback.limited")}</Label>{" "}
        {t("serverUpdate.rollback.withoutSnapshot", {
          reason: capitalize(localReasonText(snapshots) ?? t("serverUpdate.rollback.noSupport")),
        })}
      </>
    );
  }
  return (
    <>
      <Label tone="ok">{t("serverUpdate.rollback.snapshotFirst")}</Label>{" "}
      {t("serverUpdate.rollback.taken", { detail: snapshots.detail ?? "" })}
      {notCovered.length > 0 && (
        <span className={styles.note}>
          {" "}
          {t(notCovered.includes("/boot") ? "serverUpdate.rollback.notCoveredBoot" : "serverUpdate.rollback.notCovered", {
            count: notCovered.length,
            list: listWords(notCovered),
          })}
        </span>
      )}
    </>
  );
}

/**
 * The local snapshots on a server's Updates tab (ADR-0094): when each is
 * deleted, and the commands to roll back by hand. Nothing when there are
 * none.
 */
export function ServerRollback({ server }: { server: ServerDetail }) {
  const { data, error, refresh, updatedAt } = useRollback(server.id);

  if (!data) {
    return error ? (
      <ErrorState what={t("serverUpdate.snapshots.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("serverUpdate.snapshots.loading")} />
    );
  }
  if (data.local_snapshots.length === 0) return null;
  const now = updatedAt ?? new Date();
  const columns: Column<SnapshotSet>[] = [
    { key: "name", header: t("serverUpdate.snapshots.column.name"), cell: (s) => <code>{s.name}</code> },
    {
      key: "taken",
      header: t("serverUpdate.snapshots.column.taken"),
      cell: (s) => <RelativeTime value={s.created_at} now={now} />,
      sortValue: (s) => s.created_at,
    },
    { key: "covers", header: t("serverUpdate.snapshots.column.covers"), cell: covers },
    { key: "space", header: t("serverUpdate.snapshots.column.space"), cell: snapshotSpace },
    {
      key: "deleted",
      header: t("serverUpdate.snapshots.column.deleted"),
      cell: (s) => formatExact(new Date(s.delete_after)),
      sortValue: (s) => s.delete_after,
    },
  ];
  return (
    <section className={admin.section} aria-labelledby="snapshots-title">
      <div className={admin.sectionHead}>
        <h2 id="snapshots-title" className={admin.sectionTitle}>
          {t("serverUpdate.snapshots.title")}
        </h2>
      </div>
      <div className={styles.body}>
        <p className={styles.note}>{t("serverUpdate.snapshots.note")}</p>
        <DataTable
          label={t("serverUpdate.snapshots.of", { name: server.hostname })}
          columns={columns}
          rows={data.local_snapshots}
          rowKey={(s) => s.name}
          defaultOrder={(a, b) => b.created_at.localeCompare(a.created_at)}
          detailInset
          rowDetail={(s) => <Steps set={s} />}
        />
      </div>
    </section>
  );
}

/** The rollback commands of one snapshot, behind a disclosure. */
function Steps({ set }: { set: SnapshotSet }) {
  if (set.volumes.some((v) => v.invalid)) {
    return <p className={styles.note}>{t("serverUpdate.snapshots.full")}</p>;
  }
  const steps = rollbackSteps(set);
  return (
    <details className={styles.steps}>
      <summary>{t("serverUpdate.snapshots.how", { name: set.name })}</summary>
      <p className={styles.note}>{steps.intro}</p>
      <pre className={styles.commands}>{steps.commands.join("\n")}</pre>
    </details>
  );
}
