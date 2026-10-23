"use client";

import Link from "next/link";
import { Fragment, type ReactNode, useEffect, useRef, useState } from "react";

import { useUrlParams } from "@/lib/urlState";

import { Button } from "./Button";
import { cx } from "./cx";
import styles from "./DataTable.module.css";

export type SortDirection = "asc" | "desc";

export interface SortState {
  key: string;
  direction: SortDirection;
}

export interface Column<T> {
  /** Unique; also the sort key in the URL. */
  key: string;
  header: string;
  cell: (row: T) => ReactNode;
  /** Makes the header a sort button; rows compare by this value. */
  sortValue?: (row: T) => string | number | null;
  className?: string;
}

const collator = new Intl.Collator("en", { numeric: true });

function compareValues(a: string | number | null, b: string | number | null): number {
  if (a === b) return 0;
  // Missing values go last in both directions' natural reading.
  if (a === null) return 1;
  if (b === null) return -1;
  return typeof a === "number" && typeof b === "number" ? a - b : collator.compare(String(a), String(b));
}

/** `rows` in `sort` order, or in `defaultOrder` when unsorted. Does not mutate. */
export function sortRows<T>(
  rows: readonly T[],
  columns: Column<T>[],
  sort: SortState | null,
  defaultOrder?: (a: T, b: T) => number,
): T[] {
  const column = sort && columns.find((c) => c.key === sort.key && c.sortValue);
  if (!column || !sort) {
    return defaultOrder ? [...rows].sort(defaultOrder) : [...rows];
  }
  const sign = sort.direction === "asc" ? 1 : -1;
  return [...rows].sort((a, b) => sign * compareValues(column.sortValue!(a), column.sortValue!(b)));
}

/** The sort after clicking `key`'s header: ascending first, then flipping. */
export function nextSort(current: SortState | null, key: string): SortState {
  return current?.key === key && current.direction === "asc" ? { key, direction: "desc" } : { key, direction: "asc" };
}

/** `?sort=host` is ascending, `?sort=-host` descending. */
export function parseSort(value: string | null): SortState | null {
  if (!value) return null;
  return value.startsWith("-") ? { key: value.slice(1), direction: "desc" } : { key: value, direction: "asc" };
}

export function formatSort(sort: SortState | null): string | null {
  return sort && (sort.direction === "desc" ? `-${sort.key}` : sort.key);
}

/**
 * The sort order kept in the URL query `param`; null is the page's default
 * order ("Problems first"). Needs a `<Suspense>` boundary like any URL state.
 */
export function useUrlSort(param = "sort"): [SortState | null, (sort: SortState | null) => void] {
  const [params, setParams] = useUrlParams();
  return [parseSort(params.get(param)), (sort) => setParams({ [param]: formatSort(sort) })];
}

/**
 * The main cell of a row: a bold 15 px link and the 12 px grey sub-line for
 * secondary facts (OS, agent version, relay).
 */
export function PrimaryCell({ title, href, sub }: { title: ReactNode; href?: string; sub?: ReactNode }) {
  const bold = <b className={styles.title}>{title}</b>;
  return (
    <>
      {href ? <Link href={href}>{bold}</Link> : bold}
      {sub && <span className={styles.sub}>{sub}</span>}
    </>
  );
}

/**
 * A list table: 52 px rows (48 px `dense`), 40 px header, five columns or
 * fewer. Sortable columns get a `<th aria-sort>` with a button; `sort` and
 * `onSortChange` make it controlled (see `useUrlSort`), otherwise it keeps
 * its own. Without a sort, rows follow `defaultOrder` (e.g. `problemsFirst`).
 *
 * Paging: rows past `pageSize` hide behind "Load more", which shows the next
 * page and moves focus to the first link of the first new row. Give
 * `onLoadMore` and `hasMore` to fetch pages from the API instead. Change the
 * table's `key` when the filters change to start again from one page.
 */
export function DataTable<T>({
  label,
  columns,
  rows,
  rowKey,
  sort: controlledSort,
  onSortChange,
  defaultOrder,
  rowActions,
  rowMuted,
  rowDetail,
  detailInset,
  pageSize = 50,
  hasMore,
  onLoadMore,
  summary,
  loading,
  dense,
  bare,
}: {
  /** Accessible name of the table. */
  label: string;
  columns: Column<T>[];
  rows: readonly T[];
  rowKey: (row: T) => string;
  sort?: SortState | null;
  onSortChange?: (sort: SortState | null) => void;
  defaultOrder?: (a: T, b: T) => number;
  /** Buttons or links at the right end of the row. */
  rowActions?: (row: T) => ReactNode;
  /** Greys out the row (resolved, revoked). */
  rowMuted?: (row: T) => boolean;
  /** Content under the row across all columns, like command output; null for none. */
  rowDetail?: (row: T) => ReactNode;
  /** Draws `rowDetail` in the grey inset of command output and run logs. */
  detailInset?: boolean;
  pageSize?: number;
  hasMore?: boolean;
  onLoadMore?: () => void;
  /** The grey sentence beside "Load more": "Showing 50 of 212 incidents". */
  summary?: (shown: number, total: number) => ReactNode;
  /** First load: three skeleton rows of the real height. */
  loading?: boolean;
  dense?: boolean;
  /** No border of its own, inside a section box that has one. */
  bare?: boolean;
}) {
  const [ownSort, setOwnSort] = useState<SortState | null>(null);
  const sort = controlledSort === undefined ? ownSort : controlledSort;
  const setSort = onSortChange ?? setOwnSort;
  const [limit, setLimit] = useState(pageSize);
  const focusFrom = useRef<number | null>(null);
  const body = useRef<HTMLTableSectionElement>(null);

  const ordered = sortRows(rows, columns, sort, defaultOrder);
  const shown = onLoadMore ? ordered : ordered.slice(0, limit);
  const more = onLoadMore ? Boolean(hasMore) : ordered.length > limit;
  const span = columns.length + (rowActions ? 1 : 0);

  useEffect(() => {
    const from = focusFrom.current;
    if (from === null || shown.length <= from) return;
    focusFrom.current = null;
    body.current?.querySelectorAll<HTMLElement>(":scope > tr[data-row]")[from]?.querySelector<HTMLElement>("a, button")?.focus();
  });

  function loadMore() {
    focusFrom.current = shown.length;
    if (onLoadMore) {
      onLoadMore();
    } else {
      setLimit((n) => n + pageSize);
    }
  }

  return (
    <div>
      <div className={cx(styles.box, bare && styles.bare)}>
        <table className={cx(styles.table, dense && styles.dense)} aria-label={label} aria-busy={loading || undefined}>
          <thead>
            <tr>
              {columns.map((column) => {
                if (!column.sortValue) {
                  return <th key={column.key}>{column.header}</th>;
                }
                const direction = sort?.key === column.key ? sort.direction : null;
                return (
                  <th
                    key={column.key}
                    aria-sort={direction === "asc" ? "ascending" : direction === "desc" ? "descending" : "none"}
                  >
                    <button type="button" className={styles.sort} onClick={() => setSort(nextSort(sort, column.key))}>
                      {column.header}
                      {direction && <span aria-hidden="true">{direction === "asc" ? "↑" : "↓"}</span>}
                    </button>
                  </th>
                );
              })}
              {rowActions && (
                <th className={styles.actionsHead}>
                  <span className="visually-hidden">Actions</span>
                </th>
              )}
            </tr>
          </thead>
          <tbody ref={body}>
            {loading
              ? [70, 60, 76].map((width) => (
                  <tr key={width}>
                    {Array.from({ length: span }, (_, i) => (
                      <td key={i}>
                        <div className={styles.skel} style={{ width: i === 0 ? width : 50 + ((width * i) % 80) }} />
                      </td>
                    ))}
                  </tr>
                ))
              : shown.map((row) => {
                  const detail = rowDetail?.(row);
                  return (
                    <Fragment key={rowKey(row)}>
                      <tr data-row="" className={cx(rowMuted?.(row) && styles.muted)}>
                        {columns.map((column) => (
                          <td key={column.key} className={column.className}>
                            {column.cell(row)}
                          </td>
                        ))}
                        {rowActions && (
                          <td className={styles.actions}>
                            <span>{rowActions(row)}</span>
                          </td>
                        )}
                      </tr>
                      {detail != null && detail !== false && (
                        <tr className={cx(styles.detail, detailInset && styles.inset)}>
                          <td colSpan={span}>{detail}</td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })}
          </tbody>
        </table>
      </div>
      {!loading && (more || summary) && (
        <div className={styles.more}>
          {more && <Button onClick={loadMore}>Load more</Button>}
          {summary && <p className={styles.summary}>{summary(shown.length, ordered.length)}</p>}
        </div>
      )}
    </div>
  );
}
