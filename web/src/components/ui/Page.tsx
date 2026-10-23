import type { ReactNode } from "react";

import styles from "./Page.module.css";

/**
 * The column of a page inside `<main>`: title line, counters, filters,
 * tables, one under the other 20 px apart, as in every screen of the handoff.
 */
export function Page({ children }: { children: ReactNode }) {
  return <div className={styles.page}>{children}</div>;
}
