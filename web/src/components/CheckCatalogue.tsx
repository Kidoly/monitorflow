"use client";

import { useId, useMemo, useState } from "react";

import {
  type CatalogueEntry,
  type Filters,
  COMMON,
  NO_FILTERS,
  PURPOSES,
  SYSTEMS,
  TECHNOLOGIES,
  entryFacts,
  filterEntries,
} from "@/lib/catalogue";
import { Button, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";

import admin from "./AdminPages.module.css";
import styles from "./CheckCatalogue.module.css";

/**
 * Every check one can add, searchable and filtered by what it monitors,
 * the system and how it works, with the most used first. Choosing one
 * hands it to the Add a check form; a check every server runs already
 * says so instead.
 */
export function CheckCatalogue({
  entries,
  mostUsed,
  systems,
  onChoose,
  onCancel,
}: {
  entries: CatalogueEntry[];
  /** Ranked by servers running them; empty shows the common checks. */
  mostUsed: CatalogueEntry[];
  /** The systems the entries are for: one server's, or all of a site's. */
  systems: (typeof SYSTEMS)[number]["value"][];
  onChoose: (entry: CatalogueEntry) => void;
  onCancel: () => void;
}) {
  const [filters, setFilters] = useState<Filters>(NO_FILTERS);
  const [shown, setShown] = useState<CatalogueEntry | null>(null);
  const [all, setAll] = useState(false);
  const searchId = useId();
  const countId = useId();
  const matching = useMemo(() => filterEntries(entries, filters), [entries, filters]);
  const filtered = JSON.stringify(filters) !== JSON.stringify(NO_FILTERS);
  const top = mostUsed.length > 0 ? mostUsed : COMMON.map((id) => entries.find((e) => e.id === id)).filter((e) => e !== undefined);

  const pick = (entry: CatalogueEntry) => (entry.type ? onChoose(entry) : setShown(entry));
  const set = (change: Partial<Filters>) => {
    setShown(null);
    setFilters((f) => ({ ...f, ...change }));
  };

  return (
    <div className={styles.catalogue} role="group" aria-label="Check catalogue">
      <div className={styles.head}>
        <label htmlFor={searchId} className={styles.searchLabel}>
          Find a check
        </label>
        <input
          id={searchId}
          type="search"
          className={cx(fieldClass, styles.search)}
          placeholder="PostgreSQL, 443, nginx, updates…"
          value={filters.query}
          onChange={(e) => set({ query: e.target.value })}
          aria-describedby={countId}
          autoFocus
        />
        <span id={countId} className={styles.count} aria-live="polite">
          {matching.length} matching
        </span>
        <Button size="small" className={styles.cancel} onClick={onCancel}>
          Cancel
        </Button>
      </div>

      <FilterRow
        label="What to monitor"
        options={PURPOSES}
        value={filters.purpose}
        count={(purpose) => filterEntries(entries, { ...filters, purpose }).length}
        onChange={(purpose) => set({ purpose })}
      />
      {systems.length > 1 && (
        <FilterRow
          label="System"
          options={SYSTEMS.filter((s) => systems.includes(s.value))}
          value={filters.system}
          count={(system) => filterEntries(entries, { ...filters, system }).length}
          onChange={(system) => set({ system })}
        />
      )}
      <FilterRow
        label="How it works"
        options={TECHNOLOGIES}
        value={filters.technology}
        count={(technology) => filterEntries(entries, { ...filters, technology }).length}
        onChange={(technology) => set({ technology })}
      />

      {!filtered && top.length > 0 && (
        <section aria-label={mostUsed.length > 0 ? "Most used" : "Common checks"}>
          <h3 className={styles.rowTitle}>{mostUsed.length > 0 ? "Most used" : "Common checks"}</h3>
          <ul className={cx(styles.grid, styles.top)}>
            {top.map((entry) => (
              <EntryCard key={entry.id} entry={entry} onPick={pick} />
            ))}
          </ul>
        </section>
      )}

      {shown && (
        <p className={styles.always} role="status">
          {shown.title} is checked on every server already. Set when it alerts under When to alert.
        </p>
      )}

      <section aria-label={filtered ? "Matching checks" : "All checks"}>
        <h3 className={styles.rowTitle}>{filtered ? "Matching checks" : "All checks"}</h3>
        {matching.length === 0 ? (
          <p className={admin.sectionNote}>
            No check matches.{" "}
            <Button size="small" onClick={() => set(NO_FILTERS)}>
              Clear the search and filters
            </Button>{" "}
            Anything else can be checked with your own script.
          </p>
        ) : (
          <>
            <ul className={styles.grid}>
              {(filtered || all ? matching : matching.slice(0, FIRST)).map((entry) => (
                <EntryCard key={entry.id} entry={entry} onPick={pick} />
              ))}
            </ul>
            {!filtered && !all && matching.length > FIRST && (
              <Button size="small" className={styles.more} onClick={() => setAll(true)}>
                Show all {matching.length} checks
              </Button>
            )}
          </>
        )}
      </section>
    </div>
  );
}

/** Entries listed before Show all, while nothing narrows the list. */
const FIRST = 12;

function FilterRow<T extends string>({
  label,
  options,
  value,
  count,
  onChange,
}: {
  label: string;
  options: { value: T; label: string }[];
  value: T | null;
  count: (value: T) => number;
  onChange: (value: T | null) => void;
}) {
  return (
    <div className={styles.filters} role="group" aria-label={label}>
      <span className={styles.filterLabel}>{label}</span>
      <Button size="small" active={value === null} aria-pressed={value === null} onClick={() => onChange(null)}>
        All
      </Button>
      {options.map((o) => {
        const n = count(o.value);
        return (
          <Button
            key={o.value}
            size="small"
            active={value === o.value}
            aria-pressed={value === o.value}
            disabled={n === 0 && value !== o.value}
            onClick={() => onChange(value === o.value ? null : o.value)}
          >
            {o.label} <span className={styles.chipCount}>{n}</span>
          </Button>
        );
      })}
    </div>
  );
}

function EntryCard({ entry, onPick }: { entry: CatalogueEntry; onPick: (entry: CatalogueEntry) => void }) {
  return (
    <li>
      <button type="button" className={styles.card} onClick={() => onPick(entry)}>
        <span className={styles.cardTitle}>{entry.title}</span>
        <span className={styles.cardText}>{entry.description}</span>
        <span className={styles.cardFacts}>
          {entry.type ? entryFacts(entry) : "Checked on every server already"}
        </span>
      </button>
    </li>
  );
}
