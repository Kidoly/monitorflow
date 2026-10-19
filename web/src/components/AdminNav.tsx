import Link from "next/link";
import type { ReactNode } from "react";

import { type Me } from "@/lib/api";
import { isAdministrator, useMe } from "@/lib/me";
import { RELAYS_PATH, SITES_PATH, TOKENS_PATH } from "@/lib/routes";

import styles from "./Admin.module.css";

const SECTIONS = [
  { href: RELAYS_PATH, label: "Relays" },
  { href: SITES_PATH, label: "Sites" },
  { href: TOKENS_PATH, label: "Enrollment tokens" },
] as const;

export type AdminSection = (typeof SECTIONS)[number]["label"];

/**
 * The Administration sub-navigation, then `children` for administrators.
 * Other users get a note instead; the API refuses them anyway (ADR-0029).
 */
export function AdminLayout({ current, children }: { current: AdminSection; children: (me: Me) => ReactNode }) {
  const me = useMe();
  if (!me || !isAdministrator(me)) {
    return (
      <p className={styles.empty}>
        Administration needs an administrator role. Ask an administrator for one.
      </p>
    );
  }
  return (
    <>
      <nav className={styles.nav} aria-label="Administration">
        {SECTIONS.map(({ href, label }) => (
          <Link key={href} href={href} aria-current={label === current ? "page" : undefined}>
            {label}
          </Link>
        ))}
      </nav>
      {children(me)}
    </>
  );
}
