import Link from "next/link";
import type { ReactNode } from "react";

import { t } from "@/i18n";

import { cx } from "./cx";
import styles from "./StatTiles.module.css";
import { toneClass } from "./StatusLabel";
import type { Tone } from "./tone";

export interface StatTile {
  label: string;
  value: ReactNode;
  /** Shown small after the value: "13 / 17". */
  total?: ReactNode;
  /** The tint; "plain" is the grey fact tile of detail pages. */
  tone?: Tone | "plain";
  /** Makes the tile a link, usually to the list filtered on it. */
  href?: string;
  /** The filter this tile links to is the one applied. */
  active?: boolean;
  /**
   * Makes the tile a button that shows or hides a list (`expanded` with
   * `active`), the element `controls` names.
   */
  onToggle?: () => void;
  controls?: string;
}

/**
 * The counters under a list page's title. Tiles with `href` are links that
 * filter the list, tiles with `onToggle` buttons that show one below;
 * `compact` is the smaller variant of detail pages;
 * `dense` the 84 px counters of the updates, approvals and users pages.
 */
export function StatTiles({
  tiles,
  label = t("ui.tiles.label"),
  compact,
  dense,
  maxWidth = compact ? 900 : dense ? 800 : 640,
}: {
  tiles: StatTile[];
  /** Accessible name of the group. */
  label?: string;
  compact?: boolean;
  dense?: boolean;
  maxWidth?: number;
}) {
  return (
    <div
      className={cx(styles.tiles, compact && styles.compact, dense && styles.dense)}
      data-stat-tiles=""
      style={{ maxWidth }}
      role="group"
      aria-label={label}
    >
      {tiles.map((tile) => {
        const className = cx(
          styles.tile,
          tile.tone === "plain" || !tile.tone ? styles.plain : toneClass(tile.tone),
          tile.active && styles.active,
        );
        const body = (
          <>
            <span className={styles.label}>{tile.label}</span>
            <b className={styles.value}>
              {tile.value}
              {tile.total !== undefined && <span className={styles.total}> / {tile.total}</span>}
            </b>
          </>
        );
        if (tile.onToggle) {
          return (
            <button
              key={tile.label}
              type="button"
              className={cx(className, styles.button)}
              aria-expanded={tile.active ?? false}
              aria-controls={tile.active ? tile.controls : undefined}
              onClick={tile.onToggle}
            >
              {body}
            </button>
          );
        }
        return tile.href ? (
          <Link key={tile.label} href={tile.href} className={className} aria-current={tile.active ? "true" : undefined}>
            {body}
          </Link>
        ) : (
          <div key={tile.label} className={className}>
            {body}
          </div>
        );
      })}
    </div>
  );
}
