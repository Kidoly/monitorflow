"use client";

import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import { useCallback } from "react";

import { rich, t } from "@/i18n";
import { type Package, api, describeError } from "@/lib/api";
import { formatBytes, formatNumber } from "@/lib/format";
import { CAMPAIGNS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { type Column, DataTable, PrimaryCell, useUrlSort } from "./ui/DataTable";
import { Button } from "./ui/Button";
import { FilterBar, NoMatches } from "./ui/FilterBar";
import { EmptyState, ErrorState, LoadingBlock, StaleBanner } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { StatTiles } from "./ui/StatTiles";
import { Label } from "./ui/StatusLabel";
import styles from "./ServerUpdates.module.css";

/** Query parameters of the tab, prefixed so the other tabs keep theirs. */
const STATUS_PARAM = "updates";
const SEARCH_PARAM = "package";
const SORT_PARAM = "updates_sort";

type PackageStatus = "upgradable" | "current";

const TONES: Record<PackageStatus, "warn" | "ok"> = { upgradable: "warn", current: "ok" };

/** Package statuses in words. */
const packageStatus = (): Record<PackageStatus, string> => ({
  upgradable: t("server.packages.upgradable"),
  current: t("server.packages.current"),
});

export const statusOf = (p: Package): PackageStatus => (p.upgradable_version === null ? "current" : "upgradable");

/** Upgradable first, then by name and architecture. */
function upgradableFirst(a: Package, b: Package): number {
  const rank = (p: Package) => (statusOf(p) === "upgradable" ? 0 : 1);
  return rank(a) - rank(b) || a.name.localeCompare(b.name) || a.arch.localeCompare(b.arch);
}

/** Windows updates (ADR-0049) are pending or installed, by KB number. */
const windowsStatus = (): Record<PackageStatus, string> => ({
  upgradable: t("server.packages.pending"),
  current: t("server.packages.installed"),
});

/** A Windows agent's report: its entries carry a title and classification. */
export const isWindowsReport = (packages: Package[]): boolean =>
  packages.some((p) => p.title !== null || p.classification !== null);

const statusColumn = (labels: Record<PackageStatus, string>): Column<Package> => ({
  key: "status",
  header: t("server.packages.column.status"),
  sortValue: (p) => (statusOf(p) === "upgradable" ? 0 : 1),
  cell: (p) => <Label tone={TONES[statusOf(p)]}>{labels[statusOf(p)]}</Label>,
});

const columns = (): Column<Package>[] => [
  {
    key: "package",
    header: t("server.packages.column.package"),
    sortValue: (p) => p.name,
    cell: (p) => (
      <PrimaryCell
        title={p.name}
        sub={p.upgradable_version === null ? p.version : `${p.version} → ${p.upgradable_version}`}
      />
    ),
  },
  { key: "arch", header: t("server.packages.column.arch"), sortValue: (p) => p.arch, cell: (p) => p.arch },
  statusColumn(packageStatus()),
];

const windowsColumns = (): Column<Package>[] => [
  {
    key: "package",
    header: t("server.packages.column.update"),
    sortValue: (p) => p.name,
    cell: (p) => <PrimaryCell title={p.name} sub={p.title ?? ""} />,
  },
  {
    key: "classification",
    header: t("server.packages.column.classification"),
    sortValue: (p) => p.classification ?? "",
    cell: (p) => [p.classification, p.severity].filter(Boolean).join(" · "),
  },
  {
    key: "size",
    header: t("server.packages.column.size"),
    sortValue: (p) => p.size_bytes ?? 0,
    cell: (p) => (p.size_bytes === null ? "" : formatBytes(p.size_bytes)),
  },
  statusColumn(windowsStatus()),
  {
    key: "reboot",
    header: t("server.packages.column.reboot"),
    sortValue: (p) => (p.reboot_required ? 0 : 1),
    cell: (p) =>
      p.reboot_required ? (statusOf(p) === "upgradable" ? t("server.packages.mayNeed") : t("server.packages.waiting")) : "",
  },
];

/**
 * The Updates tab of a server page (`?tab=updates`): the agent's latest
 * package report, counters by status and the package table. The tab bar
 * around it belongs to the server page.
 */
export function ServerUpdates({ serverId }: { serverId: string }) {
  const fetcher = useCallback((signal: AbortSignal) => api.serverPackages(serverId, signal), [serverId]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const params = useSearchParams();
  const pathname = usePathname();
  const [sort, setSort] = useUrlSort(SORT_PARAM);

  if (!data || !updatedAt) {
    return error ? (
      <ErrorState what={t("server.packages.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("server.packages.loading")} />
    );
  }

  const windows = isWindowsReport(data.packages);
  const labels = windows ? windowsStatus() : packageStatus();
  const status = params.get(STATUS_PARAM);
  const filter = status === "upgradable" || status === "current" ? status : null;
  const search = (params.get(SEARCH_PARAM) ?? "").trim().toLowerCase();
  const upgradable = data.packages.filter((p) => statusOf(p) === "upgradable").length;
  const rows = data.packages.filter(
    (p) => (!filter || statusOf(p) === filter) && (!search || p.name.toLowerCase().includes(search)),
  );
  const filterHref = (value: PackageStatus | null) => {
    const next = new URLSearchParams(params);
    if (value) next.set(STATUS_PARAM, value);
    else next.delete(STATUS_PARAM);
    return `${pathname}?${next}`;
  };

  return (
    <div className={styles.tab}>
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {data.reported_at === null ? (
        <EmptyState
          title={t("server.packages.none")}
          actions={<Button onClick={refresh}>{t("server.packages.checkAgain")}</Button>}
        >
          {t("server.packages.noneText")}
        </EmptyState>
      ) : (
        <>
          <div className={styles.meta}>
            <span className={styles.dim}>
              {rich("server.packages.lastReport", { when: <RelativeTime value={data.reported_at} now={updatedAt} /> })}{" "}
              {windows
                ? t("server.packages.updatesListed", { count: data.packages.length })
                : t("server.packages.packagesInstalled", { count: data.packages.length })}
            </span>
            {data.reboot_required && <Label tone="warn">{t("server.packages.rebootRequired")}</Label>}
          </div>
          <StatTiles
            dense
            maxWidth={1000}
            label={t("server.packages.tiles")}
            tiles={[
              {
                label: labels.upgradable,
                value: formatNumber(upgradable),
                tone: "warn",
                href: filterHref("upgradable"),
                active: filter === "upgradable",
              },
              {
                label: labels.current,
                value: formatNumber(data.packages.length - upgradable),
                tone: "ok",
                href: filterHref("current"),
                active: filter === "current",
              },
              {
                label: windows ? t("server.packages.all") : t("server.packages.installedTile"),
                value: formatNumber(data.packages.length),
                tone: "neutral",
                href: filterHref(null),
                active: filter === null,
              },
            ]}
          />
          <FilterBar
            label={t("server.packages.filters")}
            filters={[
              {
                name: STATUS_PARAM,
                label: t("server.packages.filter.status"),
                options: [
                  { value: "upgradable", label: labels.upgradable },
                  { value: "current", label: labels.current },
                ],
              },
            ]}
            search={{
              name: SEARCH_PARAM,
              label: t("server.packages.search"),
              placeholder: windows ? t("server.packages.kb") : t("server.packages.name"),
              width: 200,
            }}
          />
          {rows.length === 0 ? (
            <NoMatches names={[STATUS_PARAM, SEARCH_PARAM]}>{t("server.packages.noMatch")}</NoMatches>
          ) : (
            <DataTable
              key={`${filter}-${search}`}
              dense
              label={t("server.packages.table")}
              columns={windows ? windowsColumns() : columns()}
              rows={rows}
              rowKey={(p) => `${p.name}:${p.arch}`}
              sort={sort}
              onSortChange={setSort}
              defaultOrder={upgradableFirst}
              summary={(shown, total) => t("server.packages.showing", { shown, total })}
            />
          )}
          <p className={styles.note}>
            {rich(windows ? "server.packages.noteWindows" : "server.packages.notePackages", {
              link: <Link href={CAMPAIGNS_PATH}>{t("server.packages.campaigns")}</Link>,
            })}
          </p>
        </>
      )}
    </div>
  );
}
