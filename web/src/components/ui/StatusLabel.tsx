import type { ReactNode } from "react";

import { cx } from "./cx";
import styles from "./StatusLabel.module.css";
import type { Tone } from "./tone";

/**
 * Every status word of the handoff and its tint, by domain: the same word
 * can take another tint elsewhere ("Revoked" is neutral for a server, bad
 * for a certificate; "Pending" is info for a token, warn for a patch).
 */
export const STATUS_WORDS = {
  server: { online: ["Online", "ok"], offline: ["Offline", "bad"], revoked: ["Revoked", "neutral"] },
  sensor: { ok: ["OK", "ok"], warning: ["Warning", "warn"], critical: ["Critical", "bad"], unknown: ["Unknown", "neutral"] },
  severity: { critical: ["Critical", "bad"], warning: ["Warning", "warn"], resolved: ["Resolved", "neutral"] },
  certificate: { revoked: ["Revoked", "bad"] },
  token: { pending: ["Pending", "info"], used: ["Used", "ok"], expired: ["Expired", "neutral"], revoked: ["Revoked", "neutral"] },
  run: {
    running: ["Running", "info"],
    succeeded: ["Succeeded", "ok"],
    failed: ["Failed", "bad"],
    queued: ["Queued", "neutral"],
    expired: ["Expired", "neutral"],
    cancelled: ["Cancelled", "neutral"],
  },
  patch: {
    pending: ["Pending", "warn"],
    approved: ["Approved", "info"],
    installed: ["Installed", "ok"],
    failed: ["Failed", "bad"],
    rejected: ["Rejected", "neutral"],
  },
  approval: { waiting: ["Waiting", "warn"], approved: ["Approved", "ok"], rejected: ["Rejected", "neutral"] },
  risk: {
    "read-only": ["Read-only", "ok"],
    "low-impact": ["Low-impact", "info"],
    "high-impact": ["High-impact", "warn"],
    forbidden: ["Forbidden", "bad"],
  },
} as const satisfies Record<string, Record<string, readonly [string, Tone]>>;

export type StatusKind = keyof typeof STATUS_WORDS;
export type StatusValue<K extends StatusKind> = keyof (typeof STATUS_WORDS)[K];

/** The word and tint of `value` in `kind`. */
export function statusWord<K extends StatusKind>(kind: K, value: StatusValue<K>): readonly [string, Tone] {
  return (STATUS_WORDS[kind] as Record<PropertyKey, readonly [string, Tone]>)[value]!;
}

/**
 * A known status as a tinted word, e.g. `<StatusLabel kind="server" value="online" />`.
 * The word is the accessible name; status is never a symbol or a colour alone.
 */
export function StatusLabel<K extends StatusKind>({
  kind,
  value,
  small,
}: {
  kind: K;
  value: StatusValue<K>;
  small?: boolean;
}) {
  const [word, tone] = statusWord(kind, value);
  return (
    <Label tone={tone} small={small}>
      {word}
    </Label>
  );
}

/** Free text in a tint, for words outside the vocabulary ("Expires in 5 days"). */
export function Label({ tone, small, children }: { tone: Tone; small?: boolean; children: ReactNode }) {
  return <span className={cx(styles.label, styles[tone], small && styles.small)}>{children}</span>;
}

/** The class of a tint alone, for blocks that take it (banners, stat tiles). */
export const toneClass = (tone: Tone) => styles[tone];
