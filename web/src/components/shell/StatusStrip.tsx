"use client";

import Link from "next/link";
import { usePathname, useSearchParams } from "next/navigation";
import { useCallback } from "react";

import { cx } from "@/components/ui/cx";
import { toneClass } from "@/components/ui/StatusLabel";
import { api } from "@/lib/api";
import { scopedHref, useScope } from "@/lib/scope";
import { SERVER_STATES, STATE_WORDS, stateHref } from "@/lib/serverState";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./StatusStrip.module.css";

const number = new Intl.NumberFormat("en");

/**
 * The servers of the scope by state, at the top of every page (ADR-0130).
 * Each count opens the fleet filtered to that state; a count that needs a
 * look is tinted, zero and OK stay plain. Nothing shows until it loads,
 * nor for a fleet without servers.
 */
export function StatusStrip() {
  const scope = useScope();
  const pathname = usePathname();
  const params = useSearchParams();
  const current = pathname === "/" ? params.get("state") : null;
  const fetcher = useCallback(
    (signal: AbortSignal) => api.serverSummary(scope, signal),
    // The scope is compared by value; a new object each render must not refetch.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [scope.customerId, scope.siteId],
  );
  const data = usePolledData(fetcher).data;
  if (!data || data.total === 0) return null;

  return (
    <nav aria-label="Servers by status" className={styles.strip}>
      <ul className={styles.list}>
        {SERVER_STATES.map((state) => {
          const count = data[state];
          const [word, tone] = STATE_WORDS[state];
          const active = current === state;
          const tinted = count > 0 && state !== "ok";
          return (
            <li key={state}>
              <Link
                href={scopedHref(active ? "/" : stateHref(state), scope)}
                className={cx(styles.item, tinted && toneClass(tone), tinted && styles.tinted, active && styles.active)}
                aria-current={active ? "page" : undefined}
              >
                <span className={styles.count}>{number.format(count)}</span> {word}
              </Link>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
