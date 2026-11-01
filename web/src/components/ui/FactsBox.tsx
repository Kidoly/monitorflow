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
}: {
  facts: Fact[];
  details?: Fact[];
  columns?: 1 | 2;
  title?: string;
}) {
  const titleId = useId();
  return (
    <section className={styles.box} aria-labelledby={title ? titleId : undefined}>
      {title && (
        <h2 id={titleId} className={styles.title}>
          {title}
        </h2>
      )}
      <Facts facts={facts} columns={columns} />
      {details.length > 0 && (
        <details className={styles.details}>
          <summary>Details</summary>
          <Facts facts={details} columns={columns} />
        </details>
      )}
    </section>
  );
}
