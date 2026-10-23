import { type ReactNode, useId } from "react";

import styles from "./TableBox.module.css";

/**
 * A bordered box with a title row and a table under it, like "Incidents on
 * this sensor" or "Timeline". Give the table `bare` so it has no border.
 */
export function TableBox({ title, meta, children }: { title: string; meta?: ReactNode; children: ReactNode }) {
  const titleId = useId();
  return (
    <section className={styles.box} aria-labelledby={titleId}>
      <div className={styles.head}>
        <h2 id={titleId} className={styles.title}>
          {title}
        </h2>
        {meta && <span className={styles.meta}>{meta}</span>}
      </div>
      {children}
    </section>
  );
}
