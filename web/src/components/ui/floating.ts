import type { CSSProperties } from "react";

/** Room kept under an anchor before a floating box opens above it instead. */
const ROOM_BELOW = 160;

/**
 * Where a `position: fixed` box opens next to `anchor`: under it, or above
 * it near the bottom of the window; aligned on its left edge, or on its
 * right one with `align: "right"`. Fixed, the box neither adds a scrollbar
 * to a scrolling table nor gets cut by it.
 */
export function floatingAt(anchor: DOMRect, gap: number, align: "left" | "right" = "left"): CSSProperties {
  const vertical =
    window.innerHeight - anchor.bottom < ROOM_BELOW && anchor.top > ROOM_BELOW
      ? { bottom: window.innerHeight - anchor.top + gap }
      : { top: anchor.bottom + gap };
  const horizontal =
    align === "right" ? { right: Math.max(8, window.innerWidth - anchor.right) } : { left: Math.max(8, anchor.left) };
  return { ...vertical, ...horizontal };
}
