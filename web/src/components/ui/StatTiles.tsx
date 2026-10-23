import Link from "next/link";
import type { ReactNode } from "react";

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
}

/**
 * The counters under a list page's title. Tiles with `href` are links that
 * filter the list; `compact` is the smaller variant of detail pages.
 */
export function StatTiles({
  tiles,
  label = "Summary, click to filter",
  compact,
  maxWidth = compact ? 900 : 640,
}: {
  tiles: StatTile[];
  /** Accessible name of the group. */
  label?: string;
  compact?: boolean;
  maxWidth?: number;
}) {
  return (
    <div className={cx(styles.tiles, compact && styles.compact)} style={{ maxWidth }} role="group" aria-label={label}>
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
