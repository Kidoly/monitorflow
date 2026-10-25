"use client";

import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import { useCallback } from "react";

import { type Package, api, describeError } from "@/lib/api";
import { formatBytes, formatNumber } from "@/lib/format";
import { CAMPAIGNS_PATH } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { type Column, DataTable, PrimaryCell, useUrlSort } from "./ui/DataTable";
import { FilterBar } from "./ui/FilterBar";
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

const STATUS: Record<PackageStatus, { label: string; tone: "warn" | "ok" }> = {
  upgradable: { label: "Upgradable", tone: "warn" },
  current: { label: "Up to date", tone: "ok" },
};

export const statusOf = (p: Package): PackageStatus => (p.upgradable_version === null ? "current" : "upgradable");

/** Upgradable first, then by name and architecture. */
function upgradableFirst(a: Package, b: Package): number {
  const rank = (p: Package) => (statusOf(p) === "upgradable" ? 0 : 1);
  return rank(a) - rank(b) || a.name.localeCompare(b.name) || a.arch.localeCompare(b.arch);
}

/** Windows updates (ADR-0049) are pending or installed, by KB number. */
const WINDOWS_STATUS: Record<PackageStatus, string> = { upgradable: "Pending", current: "Installed" };

/** A Windows agent's report: its entries carry a title and classification. */
export const isWindowsReport = (packages: Package[]): boolean =>
  packages.some((p) => p.title !== null || p.classification !== null);

const statusColumn = (labels: Record<PackageStatus, string>): Column<Package> => ({
  key: "status",
  header: "Status",
  sortValue: (p) => (statusOf(p) === "upgradable" ? 0 : 1),
  cell: (p) => <Label tone={STATUS[statusOf(p)].tone}>{labels[statusOf(p)]}</Label>,
});

const COLUMNS: Column<Package>[] = [
  {
    key: "package",
    header: "Package",
    sortValue: (p) => p.name,
    cell: (p) => (
      <PrimaryCell
        title={p.name}
        sub={p.upgradable_version === null ? p.version : `${p.version} → ${p.upgradable_version}`}
      />
    ),
  },
  { key: "arch", header: "Architecture", sortValue: (p) => p.arch, cell: (p) => p.arch },
  statusColumn({ upgradable: STATUS.upgradable.label, current: STATUS.current.label }),
];

const WINDOWS_COLUMNS: Column<Package>[] = [
  {
    key: "package",
    header: "Update",
    sortValue: (p) => p.name,
    cell: (p) => <PrimaryCell title={p.name} sub={p.title ?? ""} />,
  },
  {
    key: "classification",
    header: "Classification",
    sortValue: (p) => p.classification ?? "",
    cell: (p) => [p.classification, p.severity].filter(Boolean).join(" · "),
  },
  {
    key: "size",
    header: "Size",
    sortValue: (p) => p.size_bytes ?? 0,
    cell: (p) => (p.size_bytes === null ? "" : formatBytes(p.size_bytes)),
  },
  statusColumn(WINDOWS_STATUS),
  {
    key: "reboot",
    header: "Reboot",
    sortValue: (p) => (p.reboot_required ? 0 : 1),
    cell: (p) => (p.reboot_required ? (statusOf(p) === "upgradable" ? "May need one" : "Waiting") : ""),
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
      <ErrorState what="the package report" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading the package report" />
    );
  }

  const windows = isWindowsReport(data.packages);
  const labels = windows ? WINDOWS_STATUS : { upgradable: STATUS.upgradable.label, current: STATUS.current.label };
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
        <EmptyState title="No package report yet">
          The agent sends one when it starts, after each update and every 6 hours.
        </EmptyState>
      ) : (
        <>
          <div className={styles.meta}>
            <span className={styles.dim}>
              Last report <RelativeTime value={data.reported_at} now={updatedAt} /> ·{" "}
              {formatNumber(data.packages.length)} {windows ? "updates listed" : "packages installed"}
            </span>
            {data.reboot_required && <Label tone="warn">Reboot required</Label>}
          </div>
          <StatTiles
            dense
            maxWidth={1000}
            label="Updates by status, click to filter"
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
                label: windows ? "All" : "Installed",
                value: formatNumber(data.packages.length),
                tone: "neutral",
                href: filterHref(null),
                active: filter === null,
              },
            ]}
          />
          <FilterBar
            label="Package filters"
            filters={[
              {
                name: STATUS_PARAM,
                label: "Status",
                options: [
                  { value: "upgradable", label: labels.upgradable },
                  { value: "current", label: labels.current },
                ],
              },
            ]}
            search={{
              name: SEARCH_PARAM,
              label: "Search packages",
              placeholder: windows ? "KB number" : "Package name",
              width: 200,
            }}
          />
          {rows.length === 0 ? (
            <EmptyState>No package matches these filters.</EmptyState>
          ) : (
            <DataTable
              key={`${filter}-${search}`}
              dense
              label="Updates"
              columns={windows ? WINDOWS_COLUMNS : COLUMNS}
              rows={rows}
              rowKey={(p) => `${p.name}:${p.arch}`}
              sort={sort}
              onSortChange={setSort}
              defaultOrder={upgradableFirst}
              summary={(shown, total) => `Showing ${formatNumber(shown)} of ${formatNumber(total)} packages.`}
            />
          )}
          <p className={styles.note}>
            The agent reports its {windows ? "Windows updates" : "packages"} after each update and every 6 hours. Updates are installed by{" "}
            <Link href={CAMPAIGNS_PATH}>update campaigns</Link>.
          </p>
        </>
      )}
    </div>
  );
}
