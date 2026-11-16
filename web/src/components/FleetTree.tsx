"use client";

import Link from "next/link";
import { Fragment, type ReactNode, useState } from "react";

import { t } from "@/i18n";
import type { ServerState } from "@/lib/api";
import { SERVER_STATES, STATE_TONES, stateWord } from "@/lib/serverState";
import { Button } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";
import { type Column, type SortState, nextSort, sortRows } from "@/components/ui/DataTable";
import tableStyles from "@/components/ui/DataTable.module.css";
import { Icon } from "@/components/shell/icons";
import { toneClass } from "@/components/ui/StatusLabel";

import styles from "./FleetTree.module.css";

/** Servers an open site shows before "Show N more". */
export const GROUP_PAGE = 50;

/** States a lone server is named for in its group's pills. */
const NAMED: readonly ServerState[] = ["critical", "warning", "offline"];

/** What the tree needs of a row. */
export interface TreeRow {
  key: string;
  customer: { id: string; name: string };
  site: { id: string; name: string };
  state: ServerState;
  /** Neither a problem nor anything else to see: its site may fold to one line. */
  healthy: boolean;
  /** "Problems first" order, lower first; a group sorts by its lowest. */
  rank: number;
  /** "web-1 · Disk used /var 83%": the pill of a group's one problem. */
  problem: string;
  href: string;
}

interface Group<T> {
  key: string;
  name: string;
  rows: T[];
  rank: number;
  healthy: boolean;
}

const byName = new Intl.Collator("en", { numeric: true });

function groupBy<T extends TreeRow>(rows: T[], of: (row: T) => { id: string; name: string }): Group<T>[] {
  const groups = new Map<string, Group<T>>();
  for (const row of rows) {
    const { id, name } = of(row);
    const group = groups.get(id) ?? { key: id, name, rows: [], rank: Infinity, healthy: true };
    group.rows.push(row);
    group.rank = Math.min(group.rank, row.rank);
    group.healthy &&= row.healthy;
    groups.set(id, group);
  }
  return [...groups.values()].sort((a, b) => a.rank - b.rank || byName.compare(a.name, b.name));
}

/**
 * The fleet as a tree: customers (only for someone who sees two), sites,
 * then servers (ADR-0130). A group where every server is healthy folds to
 * one line; any other opens, as does every group while `filtered`. An open
 * site lists what needs a look and folds its healthy servers behind a
 * button, unless filtered or sorted. Columns `groupedOut` disappear while
 * grouped by site. Each
 * group line has a pill per state, naming the server when a problem state
 * has just one. One table, so the columns line up and sort across groups.
 */
export function FleetTree<T extends TreeRow>({
  rows,
  columns,
  sort,
  onSortChange,
  defaultOrder,
  multiCustomer,
  filtered,
  groupedOut = [],
  stateHref,
}: {
  rows: T[];
  columns: Column<T>[];
  sort: SortState | null;
  onSortChange: (sort: SortState | null) => void;
  defaultOrder: (a: T, b: T) => number;
  multiCustomer: boolean;
  filtered: boolean;
  /** Keys of the columns that repeat the group, like the site. */
  groupedOut?: string[];
  /** The fleet filtered to `state` within a site or a customer. */
  stateHref: (state: ServerState, place: { siteId: string } | { customerId: string }) => string;
}) {
  // Groups the user opened or closed, by key; the rest follow `healthy`.
  const [toggled, setToggled] = useState<Map<string, boolean>>(new Map());
  const [limits, setLimits] = useState<Map<string, number>>(new Map());
  const [unfolded, setUnfolded] = useState<Set<string>>(new Set());
  const isOpen = (group: Group<T>) => toggled.get(group.key) ?? (filtered || !group.healthy);
  const toggle = (group: Group<T>) => setToggled((m) => new Map(m).set(group.key, !isOpen(group)));

  const sites = groupBy(rows, (r) => r.site);
  const customers = multiCustomer ? groupBy(rows, (r) => r.customer) : null;
  const flat = !customers && sites.length === 1;
  const shownColumns = flat ? columns : columns.filter((c) => !groupedOut.includes(c.key));
  const span = shownColumns.length;

  function header(group: Group<T>, level: "customer" | "site", place: { siteId: string } | { customerId: string }) {
    const open = isOpen(group);
    return (
      <tr className={cx(styles.group, level === "customer" ? styles.customer : customers && styles.nested)}>
        <th colSpan={span} scope="rowgroup">
          <div className={styles.line}>
            <button type="button" className={styles.toggle} aria-expanded={open} onClick={() => toggle(group)}>
              <Icon name="chevron" className={cx(styles.chevron, !open && styles.folded)} />
              {group.name}
              <span className={styles.size}>
                {t("fleet.tree.servers", { count: group.rows.length })}
              </span>
            </button>
            <Pills rows={group.rows} href={(state) => stateHref(state, place)} />
          </div>
        </th>
      </tr>
    );
  }

  function servers(group: Group<T>) {
    if (!flat && !isOpen(group)) return null;
    const sorted = sortRows(group.rows, columns, sort, defaultOrder);
    // A healthy site someone opened shows its servers.
    const fold = !filtered && !sort && !group.healthy && !unfolded.has(group.key);
    const ordered = fold ? sorted.filter((r) => !r.healthy) : sorted;
    const healthy = sorted.length - ordered.length;
    const limit = limits.get(group.key) ?? GROUP_PAGE;
    const more = ordered.length - limit;
    return (
      <>
        {ordered.slice(0, limit).map((row) => (
          <tr key={row.key} data-row="">
            {shownColumns.map((column) => (
              <td key={column.key} className={cx(column.className, !flat && column === shownColumns[0] && styles.indent)}>
                {column.cell(row)}
              </td>
            ))}
          </tr>
        ))}
        {more > 0 && (
          <tr>
            <td colSpan={span} className={cx(!flat && styles.indent)}>
              <Button size="small" onClick={() => setLimits((m) => new Map(m).set(group.key, limit + GROUP_PAGE))}>
                {t("fleet.tree.more", { count: Math.min(more, GROUP_PAGE), group: group.name })}
              </Button>
            </td>
          </tr>
        )}
        {more <= 0 && healthy > 0 && (
          <tr>
            <td colSpan={span} className={cx(!flat && styles.indent)}>
              <Button size="small" onClick={() => setUnfolded((u) => new Set(u).add(group.key))}>
                {flat
                  ? t("fleet.tree.ok", { count: healthy })
                  : t("fleet.tree.okOf", { count: healthy, group: group.name })}
              </Button>
            </td>
          </tr>
        )}
      </>
    );
  }

  const site = (group: Group<T>) => (
    <tbody key={group.key}>
      {!flat && header(group, "site", { siteId: group.key })}
      {servers(group)}
    </tbody>
  );

  return (
    <div className={tableStyles.box}>
      <table className={cx(tableStyles.table, styles.tree)} aria-label={t("fleet.table")}>
        <thead>
          <tr>
            {shownColumns.map((column) => {
              if (!column.sortValue) return <th key={column.key}>{column.header}</th>;
              const direction = sort?.key === column.key ? sort.direction : null;
              return (
                <th key={column.key} aria-sort={direction === "asc" ? "ascending" : direction === "desc" ? "descending" : "none"}>
                  <button type="button" className={tableStyles.sort} onClick={() => onSortChange(nextSort(sort, column.key))}>
                    {column.header}
                    {direction && <span aria-hidden="true">{direction === "asc" ? "↑" : "↓"}</span>}
                  </button>
                </th>
              );
            })}
          </tr>
        </thead>
        {customers
          ? customers.map((customer) => (
              <Fragment key={customer.key}>
                <tbody>{header(customer, "customer", { customerId: customer.key })}</tbody>
                {isOpen(customer) &&
                  sites.filter((s) => s.rows[0]!.customer.id === customer.key).map(site)}
              </Fragment>
            ))
          : sites.map(site)}
      </table>
    </div>
  );
}

/**
 * A pill per state present: "3 critical", or "web-1 · Disk used /var 83%"
 * when a problem state has one server. OK stays plain.
 */
function Pills<T extends TreeRow>({ rows, href }: { rows: T[]; href: (state: ServerState) => string }) {
  const pills: ReactNode[] = [];
  for (const state of SERVER_STATES) {
    const of = rows.filter((r) => r.state === state);
    if (of.length === 0) continue;
    const word = stateWord(state);
    const tone = STATE_TONES[state];
    const lone = of.length === 1 && NAMED.includes(state) ? of[0]! : null;
    pills.push(
      <li key={state}>
        <Link
          href={lone ? lone.href : href(state)}
          className={cx(styles.pill, state !== "ok" && toneClass(tone), state !== "ok" && styles.tinted)}
        >
          {lone ? (
            <>
              <span className="visually-hidden">{t("fleet.tree.lone", { state: word })}</span>
              {lone.problem}
            </>
          ) : (
            <>
              <b>{of.length}</b> {t(`fleet.tree.pill.${state}`)}
            </>
          )}
        </Link>
      </li>,
    );
  }
  return <ul className={styles.pills}>{pills}</ul>;
}
