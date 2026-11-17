import type { ReactNode } from "react";

import { type PlainKey, t } from "@/i18n";

import { cx } from "./cx";
import styles from "./StatusLabel.module.css";
import type { Tone } from "./tone";

/**
 * Every status of the handoff and its tint, by domain; the words are the
 * catalog's `ui.status.<kind>.<value>`. The same word can take another tint
 * elsewhere ("Removed" is neutral for a server, "Revoked" bad for a
 * certificate; "Pending" is info for a token, warn for a patch).
 */
export const STATUS_TONES = {
  server: { online: "ok", offline: "bad", revoked: "neutral", waiting: "neutral" },
  sensor: { ok: "ok", warning: "warn", critical: "bad", unknown: "neutral" },
  severity: { critical: "bad", warning: "warn", resolved: "neutral" },
  certificate: { revoked: "bad" },
  token: { pending: "info", used: "ok", expired: "neutral", revoked: "neutral" },
  installKey: { active: "ok", expired: "neutral", used_up: "neutral", revoked: "neutral" },
  run: {
    running: "info",
    sent: "info",
    succeeded: "ok",
    failed: "bad",
    queued: "neutral",
    expired: "neutral",
    cancelled: "neutral",
  },
  patch: { pending: "warn", approved: "info", installed: "ok", failed: "bad", rejected: "neutral" },
  remediation: {
    open: "warn",
    running: "info",
    awaiting_approval: "warn",
    resolved: "ok",
    escalated: "bad",
    failed: "bad",
  },
  approval: { waiting: "warn", approved: "ok", rejected: "neutral" },
  risk: { "read-only": "ok", "low-impact": "info", "high-impact": "warn", forbidden: "bad" },
} as const satisfies Record<string, Record<string, Tone>>;

export type StatusKind = keyof typeof STATUS_TONES;
export type StatusValue<K extends StatusKind> = keyof (typeof STATUS_TONES)[K];

/** The word, in the active language, and tint of `value` in `kind`. */
export function statusWord<K extends StatusKind>(kind: K, value: StatusValue<K>): readonly [string, Tone] {
  const tone = (STATUS_TONES[kind] as Record<PropertyKey, Tone>)[value]!;
  return [t(`ui.status.${kind}.${String(value)}` as PlainKey), tone];
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
