import Link from "next/link";
import { Fragment } from "react";

import styles from "./Breadcrumb.module.css";

/**
 * "Fleet / web-01": links to the parents, then the current page, which
 * carries `aria-current="page"`.
 */
export function Breadcrumb({ parents, current }: { parents: { label: string; href: string }[]; current: string }) {
  return (
    <nav className={styles.crumb} aria-label="Breadcrumb">
      {parents.map((parent) => (
        <Fragment key={parent.href}>
          <Link href={parent.href}>{parent.label}</Link>
          <span aria-hidden="true">/</span>
        </Fragment>
      ))}
      <span className={styles.current} aria-current="page">
        {current}
      </span>
    </nav>
  );
}
