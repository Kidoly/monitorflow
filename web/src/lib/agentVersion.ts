/**
 * Agent versions (ADR-0053): `major.minor.patch` with an optional
 * pre-release suffix, ordered as the agent orders them (Semantic
 * Versioning). Anything else does not compare.
 */
type Parsed = { core: [number, number, number]; pre: string[] };

const VERSION =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/;

function parse(version: string): Parsed | null {
  const m = VERSION.exec(version);
  if (!m) return null;
  return {
    core: [Number(m[1]), Number(m[2]), Number(m[3])],
    pre: m[4] ? m[4].split(".") : [],
  };
}

const numeric = (id: string) => /^\d+$/.test(id);

function compareIds(a: string, b: string): number {
  if (numeric(a) && numeric(b))
    return a.length - b.length || (a < b ? -1 : a > b ? 1 : 0);
  if (numeric(a) !== numeric(b)) return numeric(a) ? -1 : 1;
  return a < b ? -1 : a > b ? 1 : 0;
}

/** Negative when `a` is older than `b`; null when either is not a version. */
export function compareVersions(a: string, b: string): number | null {
  const [x, y] = [parse(a), parse(b)];
  if (!x || !y) return null;
  for (let i = 0; i < 3; i++)
    if (x.core[i] !== y.core[i]) return x.core[i]! - y.core[i]!;
  if (x.pre.length === 0 || y.pre.length === 0)
    return y.pre.length - x.pre.length;
  for (let i = 0; i < Math.min(x.pre.length, y.pre.length); i++) {
    const order = compareIds(x.pre[i]!, y.pre[i]!);
    if (order !== 0) return order;
  }
  return x.pre.length - y.pre.length;
}

/** Whether an agent running `current` is older than `latest`. */
export function updateAvailable(
  current: string | null,
  latest: string | null,
): boolean {
  if (!current || !latest) return false;
  return (compareVersions(current, latest) ?? 0) < 0;
}
