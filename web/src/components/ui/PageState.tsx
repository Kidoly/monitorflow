import type { ReactNode } from "react";

import { cx } from "./cx";
import styles from "./PageState.module.css";
import { toneClass } from "./StatusLabel";
import { formatClock } from "./time";
import type { Tone } from "./tone";

/**
 * A full-width message: `bad` errors (role alert), `warn` stale data (role
 * status), `info` notes, `plain` grey remarks. No icons. `role` overrides
 * the tone's.
 */
export function Banner({
  tone,
  role = tone === "bad" ? "alert" : tone === "warn" ? "status" : undefined,
  children,
}: {
  tone: Tone | "plain";
  role?: "alert" | "status";
  children: ReactNode;
}) {
  return (
    <div className={cx(styles.banner, tone === "plain" ? styles.plain : toneClass(tone))} role={role}>
      <div>{children}</div>
    </div>
  );
}

function Retry({ onRetry }: { onRetry?: () => void }) {
  return onRetry ? (
    <>
      {" "}
      <button type="button" className={styles.retry} onClick={onRetry}>
        Retry now
      </button>
    </>
  ) : null;
}

/**
 * The title line of every page: title, optional status label, the grey
 * refresh line (announced politely) and actions at the right. `detail`
 * is the 26 px variant of detail pages.
 */
export function PageHeader({
  title,
  status,
  meta,
  actions,
  detail,
}: {
  title: ReactNode;
  status?: ReactNode;
  meta?: ReactNode;
  actions?: ReactNode;
  detail?: boolean;
}) {
  return (
    <div className={cx(styles.header, detail && styles.detail)}>
      <h1 className={styles.title}>{title}</h1>
      {status}
      {meta !== undefined && (
        <span className={styles.meta} aria-live="polite">
          {meta}
        </span>
      )}
      {actions && <div className={styles.actions}>{actions}</div>}
    </div>
  );
}

/**
 * "Updated 14:32:05, refreshes every 15 seconds", with the time in the
 * warning colour when the data is stale.
 */
export function RefreshLine({ updatedAt, stale, intervalMs = 15_000 }: { updatedAt: Date | null; stale?: boolean; intervalMs?: number }) {
  const every = `refreshes every ${Math.round(intervalMs / 1000)} seconds`;
  if (!updatedAt) return <>Could not load. Retrying every {Math.round(intervalMs / 1000)} seconds.</>;
  const time = `Updated ${formatClock(updatedAt)}`;
  return (
    <>
      {stale ? <strong className={styles.stale}>{time}</strong> : time}, {every}
    </>
  );
}

/** Skeleton bars for a first load: never a spinner, never on refresh. */
export function LoadingBlock({ label = "Loading" }: { label?: string }) {
  return (
    <div className={styles.skeleton} aria-busy="true" aria-label={label} role="status">
      <div className={styles.skel} style={{ width: "40%" }} />
      <div className={styles.skel} style={{ width: "90%" }} />
      <div className={styles.skel} style={{ width: "75%" }} />
    </div>
  );
}

/** Nothing to list yet: a title, one sentence, and usually the action that fixes it. */
export function EmptyState({ title, children, actions }: { title?: string; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className={cx(styles.empty, !title && styles.compactEmpty)}>
      {title && <strong className={styles.emptyTitle}>{title}</strong>}
      {children && <span>{children}</span>}
      {actions && <div className={styles.emptyActions}>{actions}</div>}
    </div>
  );
}

/** The first load failed: the reason, automatic retry, and an empty box in place of the data. */
export function ErrorState({ what, error, onRetry }: { what: string; error: string; onRetry?: () => void }) {
  return (
    <>
      <Banner tone="bad">
        <strong>Could not load {what}:</strong> {error} Retrying automatically.
        <Retry onRetry={onRetry} />
      </Banner>
      <EmptyState>Nothing to show until the first successful load.</EmptyState>
    </>
  );
}

/**
 * A refresh failed but earlier data is still shown, untouched, under this
 * banner. `failures` counts the refreshes failed in a row.
 */
export function StaleBanner({
  error,
  updatedAt,
  failures,
  onRetry,
}: {
  error: string;
  updatedAt: Date;
  failures: number;
  onRetry?: () => void;
}) {
  return (
    <Banner tone="warn">
      <strong>Refresh failed:</strong> {error} Showing the last data received, from {formatClock(updatedAt)}.{" "}
      {failures} {failures === 1 ? "attempt" : "attempts"} failed.
      <Retry onRetry={onRetry} />
    </Banner>
  );
}

/**
 * A missing or out-of-scope object, with the same words for both so the
 * page never reveals whether the ID exists.
 */
export function NotFound({ title, actions, children }: { title: string; actions?: ReactNode; children?: ReactNode }) {
  return (
    <EmptyState title={title} actions={actions}>
      {children ?? "It may have been removed, or your roles do not cover its site."}
    </EmptyState>
  );
}

/** A page the user's roles do not open; reached only by a direct link. */
export function NoPermission({ title, children }: { title: string; children?: ReactNode }) {
  return <EmptyState title={title}>{children}</EmptyState>;
}
