import { type ReactNode, useId } from "react";

import type { PanelKind } from "@/lib/api";
import { kindLabel } from "@/lib/dashboards";

import styles from "./KindTiles.module.css";

/** A small picture of each kind of panel, in the text colour and one accent. */
const PICTURES: Record<PanelKind, ReactNode> = {
  chart: (
    <>
      <polyline points="4,24 14,16 22,20 32,8 44,12" />
      <polyline className={styles.second} points="4,28 14,24 22,26 32,20 44,22" />
    </>
  ),
  area: (
    <>
      <polygon className={styles.fill} points="4,28 4,20 14,14 24,18 34,8 44,12 44,28" />
      <polyline points="4,20 14,14 24,18 34,8 44,12" />
    </>
  ),
  columns: (
    <>
      {[6, 14, 22, 30, 38].map((x, i) => (
        <rect key={x} className={styles.fill} x={x} y={[18, 12, 20, 8, 14][i]} width="5" height={28 - [18, 12, 20, 8, 14][i]!} />
      ))}
    </>
  ),
  heatmap: (
    <>
      {[0, 1, 2].flatMap((r) =>
        [0, 1, 2, 3, 4, 5].map((c) => (
          <rect key={`${r}-${c}`} className={styles.fill} x={4 + c * 7} y={6 + r * 7.5} width="6" height="6.5" opacity={[0.25, 0.5, 0.9, 0.4, 0.7, 0.3][(r + c) % 6]} />
        )),
      )}
    </>
  ),
  figure: (
    <text x="24" y="23" textAnchor="middle" className={styles.figure}>
      42
    </text>
  ),
  gauge: (
    <>
      <path d="M 10 26 A 14 14 0 1 1 38 26" className={styles.track} />
      <path d="M 10 26 A 14 14 0 0 1 32 14" />
    </>
  ),
  top: (
    <>
      {[8, 16, 24].map((y) => (
        <g key={y}>
          <circle cx="8" cy={y} r="1.6" className={styles.dot} />
          <line x1="13" y1={y} x2="40" y2={y} />
        </g>
      ))}
    </>
  ),
  bars: (
    <>
      {[36, 28, 18].map((w, i) => (
        <rect key={w} className={styles.fill} x="6" y={6 + i * 8} width={w} height="5" rx="1" />
      ))}
    </>
  ),
  table: (
    <>
      <rect x="5" y="5" width="38" height="22" className={styles.track} />
      <line x1="5" y1="12" x2="43" y2="12" />
      <line x1="5" y1="19" x2="43" y2="19" />
      <line x1="19" y1="5" x2="19" y2="27" />
    </>
  ),
  states: (
    <>
      {[0, 1, 2].flatMap((r) =>
        [0, 1, 2, 3, 4].map((c) => (
          <rect
            key={`${r}-${c}`}
            x={6 + c * 7.6}
            y={5 + r * 7.6}
            width="6"
            height="6"
            rx="1"
            className={r === 0 && c === 1 ? styles.bad : r === 2 && c === 3 ? styles.warn : styles.ok}
          />
        )),
      )}
    </>
  ),
  pie: (
    <>
      <circle cx="24" cy="16" r="10" className={styles.ring} />
      <path d="M 24 6 A 10 10 0 0 1 33.5 19" className={styles.slice} />
    </>
  ),
  timeline: (
    <>
      {[9, 17, 25].map((y) => (
        <line key={y} x1="5" y1={y} x2="43" y2={y} className={styles.okLine} />
      ))}
      <line x1="16" y1="9" x2="24" y2="9" className={styles.badLine} />
      <line x1="30" y1="17" x2="36" y2="17" className={styles.warnLine} />
    </>
  ),
  alerts: (
    <>
      <path d="M 24 5 L 36 27 L 12 27 Z" className={styles.track} />
      <line x1="24" y1="12" x2="24" y2="20" />
      <circle cx="24" cy="23.5" r="1.2" className={styles.dot} />
    </>
  ),
  text: (
    <>
      <line x1="6" y1="8" x2="30" y2="8" className={styles.thick} />
      <line x1="6" y1="16" x2="42" y2="16" />
      <line x1="6" y1="22" x2="36" y2="22" />
    </>
  ),
};

/** The picture of `kind`, for tiles and empty states. */
export function KindPicture({ kind }: { kind: PanelKind }) {
  return (
    <svg className={styles.picture} viewBox="0 0 48 32" aria-hidden="true">
      {PICTURES[kind]}
    </svg>
  );
}

/**
 * Ways to draw a panel as tiles with a picture, one of them chosen: a
 * group of radio buttons, so the keyboard moves between them.
 */
export function KindTiles({
  kinds,
  value,
  onChange,
  label,
}: {
  kinds: readonly PanelKind[];
  value: PanelKind;
  onChange: (kind: PanelKind) => void;
  label: string;
}) {
  const name = useId();
  return (
    <div className={styles.tiles} role="radiogroup" aria-label={label}>
      {kinds.map((kind) => (
        <label key={kind} className={styles.tile} data-checked={kind === value || undefined}>
          <input className={styles.radio} type="radio" name={name} checked={kind === value} onChange={() => onChange(kind)} />
          <KindPicture kind={kind} />
          <span>{kindLabel(kind)}</span>
        </label>
      ))}
    </div>
  );
}
