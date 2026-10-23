"use client";

import { type ReactNode, useState } from "react";

import { type ParamChanges, useUrlParams } from "@/lib/urlState";

import { buttonClass, fieldClass } from "./Button";
import { cx } from "./cx";
import styles from "./FilterBar.module.css";

/** A 36 px control backed by a native `<select>`; the empty choice is "any". */
export interface SelectFilter {
  kind?: "select";
  /** Query parameter, e.g. `status`. */
  name: string;
  /** Shown when unset, and before the choice: "Status: Online". */
  label: string;
  options: { value: string; label: string }[];
}

/** A button that sets `name=value` when pressed ("Has incidents ×"). */
export interface ToggleFilter {
  kind: "toggle";
  name: string;
  label: string;
  /** The value written when on; defaults to "1". */
  value?: string;
}

export type Filter = SelectFilter | ToggleFilter;

export interface SearchField {
  name: string;
  /** Accessible name of the input. */
  label: string;
  placeholder?: string;
  width?: number;
}

/**
 * The filter row of a list page. Every change goes into the URL with
 * `history.replaceState` (shareable, no history entry), then `onChange` gets
 * the new query so the page can refetch. "Clear" removes only the keys this
 * bar owns, so the scope (`?customer=`) and page keys (`?id=`) stay.
 */
export function FilterBar({
  filters,
  search,
  onChange,
  trailing,
  label = "Filters",
}: {
  filters: Filter[];
  search?: SearchField;
  onChange?: (params: URLSearchParams) => void;
  /** Controls at the right end, like a sort toggle. */
  trailing?: ReactNode;
  label?: string;
}) {
  const [params, setParams] = useUrlParams();
  const keys = [...(search ? [search.name] : []), ...filters.map((f) => f.name)];
  // The input keeps its own state: Next applies replaceState to
  // useSearchParams in a transition, which would lag behind typing.
  const [text, setText] = useState(() => (search && params.get(search.name)) ?? "");
  const anySet = text !== "" || filters.some((f) => params.get(f.name));

  function update(changes: ParamChanges) {
    setParams(changes);
    onChange?.(new URLSearchParams(window.location.search));
  }

  return (
    <form className={styles.bar} aria-label={label} onSubmit={(e) => e.preventDefault()}>
      {search && (
        <input
          className={cx(fieldClass, styles.search)}
          style={search.width ? { width: search.width } : undefined}
          type="search"
          aria-label={search.label}
          placeholder={search.placeholder}
          value={text}
          onChange={(e) => {
            setText(e.target.value);
            update({ [search.name]: e.target.value });
          }}
        />
      )}
      {filters.map((filter) => {
        const current = params.get(filter.name);
        if (filter.kind === "toggle") {
          const on = current === (filter.value ?? "1");
          return (
            <button
              key={filter.name}
              type="button"
              className={buttonClass({ active: on })}
              aria-pressed={on}
              onClick={() => update({ [filter.name]: on ? null : (filter.value ?? "1") })}
            >
              {filter.label}
              {on && <span aria-hidden="true"> ×</span>}
            </button>
          );
        }
        const known = filter.options.some((o) => o.value === current);
        return (
          <select
            key={filter.name}
            className={cx(buttonClass({ active: known }), styles.select)}
            aria-label={filter.label}
            value={known ? current! : ""}
            onChange={(e) => update({ [filter.name]: e.target.value })}
          >
            <option value="">{filter.label}</option>
            {filter.options.map((option) => (
              <option key={option.value} value={option.value}>
                {filter.label}: {option.label}
              </option>
            ))}
          </select>
        );
      })}
      {anySet && (
        <button type="button" className={styles.clear} onClick={() => {
            setText("");
            update(Object.fromEntries(keys.map((k) => [k, null])));
          }}>
          Clear
        </button>
      )}
      {trailing && <span className={styles.trailing}>{trailing}</span>}
    </form>
  );
}
