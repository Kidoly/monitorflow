/** What "problems first" orders rows by, most urgent first. */
export interface ProblemFacts {
  /** Open critical incidents. */
  critical: number;
  /** Open warning incidents. */
  warning: number;
  offline: boolean;
  /** Certificate expiring soon. */
  certificateWarning: boolean;
  /** Tie-breaker, compared as text: the hostname. */
  name: string;
}

/**
 * 0 for an open critical, 1 open warning, 2 offline, 3 certificate warning,
 * 4 healthy: the handoff's default order of list pages.
 */
export function problemRank(facts: ProblemFacts): number {
  if (facts.critical > 0) return 0;
  if (facts.warning > 0) return 1;
  if (facts.offline) return 2;
  if (facts.certificateWarning) return 3;
  return 4;
}

/** Whether the row has none of the problems, so it may be collapsed. */
export function isHealthy(facts: ProblemFacts): boolean {
  return problemRank(facts) === 4;
}

const byName = new Intl.Collator("en", { numeric: true });

/**
 * A comparator for `rows.sort` or `DataTable`'s `defaultOrder`: problems
 * first, then hostname with numbers in order (web-2 before web-10).
 */
export function problemsFirst<T>(facts: (row: T) => ProblemFacts): (a: T, b: T) => number {
  return (a, b) => {
    const fa = facts(a);
    const fb = facts(b);
    return problemRank(fa) - problemRank(fb) || byName.compare(fa.name, fb.name);
  };
}
