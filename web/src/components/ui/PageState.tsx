import type { ReactNode } from "react";

import { t } from "@/i18n";

import { Button, ButtonLink } from "./Button";
import { cx } from "./cx";
import styles from "./PageState.module.css";
import { toneClass } from "./StatusLabel";
import { formatAgo, formatClock } from "./time";
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
        {t("ui.page.retry")}
      </button>
    </>
  ) : null;
}

/**
 * The title line of every page: title, optional status label, the grey
 * refresh line (announced politely) and actions at the right. `detail`
 * is the 26 px variant of detail pages. A `tone` tints the line by status,
 * and `reason` says why in one sentence under the title.
 */
export function PageHeader({
  title,
  status,
  meta,
  actions,
  detail,
  tone,
  reason,
  sub,
}: {
  title: ReactNode;
  status?: ReactNode;
  meta?: ReactNode;
  actions?: ReactNode;
  detail?: boolean;
  tone?: Tone;
  reason?: ReactNode;
  /** One line under the title: the facts that matter, badges, a Details toggle. */
  sub?: ReactNode;
}) {
  // An OK state stays quiet: only a problem tints the header.
  const tinted = tone !== undefined && tone !== "ok";
  return (
    <div className={cx(styles.header, detail && styles.detail, tinted && styles.tinted, tinted && styles[`tint-${tone}`])}>
      <h1 className={styles.title}>{title}</h1>
      {status}
      {meta !== undefined && (
        <span className={styles.meta} aria-live="polite">
          {meta}
        </span>
      )}
      {actions && <div className={styles.actions}>{actions}</div>}
      {reason && <p className={styles.reason}>{reason}</p>}
      {sub && <div className={styles.sub}>{sub}</div>}
    </div>
  );
}

/**
 * Nothing while the data is fresh: refreshing is the normal state. When a
 * refresh failed, a muted "Updated 3 min ago"; before any load, the retry.
 */
export function RefreshLine({ updatedAt, stale, intervalMs = 15_000 }: { updatedAt: Date | null; stale?: boolean; intervalMs?: number }) {
  if (!updatedAt) return <>{t("ui.page.retrying", { count: Math.round(intervalMs / 1000) })}</>;
  if (!stale) return null;
  return <span className={styles.stale}>{t("ui.page.updatedAgo", { ago: formatAgo(updatedAt, new Date()) })}</span>;
}

/** Skeleton bars for a first load: never a spinner, never on refresh. */
export function LoadingBlock({ label = t("ui.page.loading") }: { label?: string }) {
  return (
    <div className={styles.skeleton} aria-busy="true" aria-label={label} role="status">
      <div className={styles.skel} style={{ width: "40%" }} />
      <div className={styles.skel} style={{ width: "90%" }} />
      <div className={styles.skel} style={{ width: "75%" }} />
    </div>
  );
}

/** Skeleton lines inside a box that already exists (a menu, a log, a panel). */
export function SkeletonLines({ label = t("ui.page.loading"), lines = 3 }: { label?: string; lines?: number }) {
  return (
    <div className={styles.skelLines} aria-busy="true" aria-label={label} role="status">
      {Array.from({ length: lines }, (_, i) => (
        <div key={i} className={styles.skel} style={{ width: `${[60, 90, 75, 85][i % 4]}%` }} />
      ))}
    </div>
  );
}

/** Nothing to list yet: a title, one sentence, and the action that comes next. */
export function EmptyState({ title, children, actions }: { title?: string; children?: ReactNode; actions?: ReactNode }) {
  return (
    <div className={cx(styles.empty, !title && styles.compactEmpty)}>
      {title && <strong className={styles.emptyTitle}>{title}</strong>}
      {children && <div>{children}</div>}
      {actions && <div className={styles.emptyActions}>{actions}</div>}
    </div>
  );
}

/** The first load failed: the reason, automatic retry, and an empty box in place of the data. */
export function ErrorState({ what, error, onRetry }: { what: string; error: string; onRetry?: () => void }) {
  return (
    <>
      <Banner tone="bad">
        <strong>{t("ui.page.loadFailed", { what })}</strong> {error} {t("ui.page.retryingAutomatically")}
      </Banner>
      <EmptyState actions={onRetry && <Button onClick={onRetry}>{t("ui.page.retry")}</Button>}>
        {t("ui.page.nothingYet")}
      </EmptyState>
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
      <strong>{t("ui.page.refreshFailed")}</strong> {error} {t("ui.page.showingLast", { time: formatClock(updatedAt) })}{" "}
      {t("ui.page.attemptsFailed", { count: failures })}
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
      {children ?? t("ui.page.notFound")}
    </EmptyState>
  );
}

/** A page the user's roles do not open; reached only by a direct link. */
export function NoPermission({ title, children }: { title: string; children?: ReactNode }) {
  return (
    <EmptyState title={title} actions={<ButtonLink href="/">{t("ui.page.backToFleet")}</ButtonLink>}>
      {children}
    </EmptyState>
  );
}
