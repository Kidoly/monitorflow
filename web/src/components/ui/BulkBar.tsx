"use client";

import { type ReactNode, useEffect, useRef } from "react";

import { t } from "@/i18n";
import type { BulkSummary } from "@/lib/bulk";

import { Button } from "./Button";
import { cx } from "./cx";
import styles from "./BulkBar.module.css";
import { toneClass } from "./StatusLabel";

/**
 * The bar over a table once rows are checked: "3 selected", the actions
 * that apply to them, and Deselect. It stays to say how the last action
 * went ("4 acknowledged, 1 refused: insufficient rights"), in a live
 * region screen readers announce. It sticks to the top while the table
 * scrolls under it. Put it in one block with its table, so it takes no
 * gap of the page while empty.
 */
export function BulkBar({
  count,
  onClear,
  summary,
  onDismiss,
  children,
}: {
  count: number;
  onClear: () => void;
  summary: BulkSummary | null;
  onDismiss: () => void;
  /** The actions, shown while at least one row is checked. */
  children: ReactNode;
}) {
  const shown = count > 0 || summary !== null;
  return (
    <div
      className={cx(styles.bar, !shown && styles.hidden)}
      role="region"
      aria-label={t("ui.bulk.label")}
    >
      {count > 0 && (
        <div className={styles.line}>
          <strong className={styles.count}>
            {t("ui.bulk.selected", { count })}
          </strong>
          <div className={styles.actions}>{children}</div>
          <Button size="small" variant="ghost" onClick={onClear}>
            {t("ui.bulk.clear")}
          </Button>
        </div>
      )}
      <p
        role="status"
        className={cx(styles.summary, summary && toneClass(summary.tone))}
      >
        {summary && (
          <>
            {summary.text}{" "}
            <button
              type="button"
              className={styles.dismiss}
              onClick={onDismiss}
            >
              {t("ui.bulk.dismiss")}
            </button>
          </>
        )}
      </p>
    </div>
  );
}

/**
 * A row's checkbox: "Select jellyfin". With `keys`, the header's: checked
 * when every row shown is, mixed when some are, and a click checks or
 * unchecks the rows shown only.
 */
export function SelectBox({
  label,
  keys,
  selected,
  onChange,
}: {
  label: string;
  keys: readonly string[];
  selected: ReadonlySet<string>;
  onChange: (keys: readonly string[], on: boolean) => void;
}) {
  const ref = useRef<HTMLInputElement>(null);
  const on = keys.filter((k) => selected.has(k)).length;
  const all = keys.length > 0 && on === keys.length;
  useEffect(() => {
    if (ref.current) ref.current.indeterminate = on > 0 && !all;
  });
  return (
    <input
      ref={ref}
      type="checkbox"
      className={styles.box}
      aria-label={label}
      checked={all}
      disabled={keys.length === 0}
      onChange={() => onChange(keys, !all)}
    />
  );
}
