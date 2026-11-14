import { type ReactNode, useId } from "react";

import { cx } from "./cx";
import styles from "./FactsBox.module.css";

export interface Fact {
  label: string;
  value: ReactNode;
  /** IDs, serials, commands. */
  mono?: boolean;
}

function Facts({ facts, columns }: { facts: Fact[]; columns: 1 | 2 }) {
  return (
    <dl className={cx(styles.facts, columns === 2 && styles.two)}>
      {facts.map((fact) => (
        <div key={fact.label} style={{ display: "contents" }}>
          <dt>{fact.label}</dt>
          <dd className={cx(fact.mono && styles.mono)}>{fact.value}</dd>
        </div>
      ))}
    </dl>
  );
}

/**
 * The plain facts list of a detail page: grey label, black value, nothing
 * else. `columns={2}` puts two pairs per line, down to one at ≤ 1280 px.
 * `details` are the technical facts, closed under a "Details" disclosure.
 */
export function FactsBox({
  facts,
  details = [],
  columns = 1,
  title,
  actions,
  children,
}: {
  facts: Fact[];
  details?: Fact[];
  columns?: 1 | 2;
  title?: string;
  /** Buttons on the title line, like Edit. */
  actions?: ReactNode;
  /** Shown instead of the facts, like the form that edits them. */
  children?: ReactNode;
}) {
  const titleId = useId();
  return (
    <section className={styles.box} aria-labelledby={title ? titleId : undefined}>
      {(title || actions) && (
        <div className={styles.head}>
          {title && (
            <h2 id={titleId} className={styles.title}>
              {title}
            </h2>
          )}
          {actions && <div className={styles.actions}>{actions}</div>}
        </div>
      )}
      {children || <Facts facts={facts} columns={columns} />}
      {!children && details.length > 0 && (
        <details className={styles.details}>
          <summary>Details</summary>
          <Facts facts={details} columns={columns} />
        </details>
      )}
    </section>
  );
}
