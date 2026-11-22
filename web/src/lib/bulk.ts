import { useCallback, useState } from "react";

import { t } from "@/i18n";

import { ApiError, describeError } from "./api";

/** What one bulk action did to one item. */
export type BulkOutcome =
  | { kind: "done" }
  /** Nothing to do: already done, or it does not apply. */
  | { kind: "skipped" }
  /** The user's roles do not let them change it. */
  | { kind: "refused" }
  | { kind: "failed"; error: string };

/** An action applied to each selected item on its own, through the API's unit routes. */
export interface BulkAction<T> {
  /** "4 acknowledged". */
  done: (count: number) => string;
  /** "1 already acknowledged or resolved"; the generic words when absent. */
  skipped?: (count: number) => string;
  /** Whether the user may change it; the others are refused without asking the API. */
  allowed?: (item: T) => boolean;
  /** Acts on one item; rejecting fails that item only. */
  act: (item: T) => Promise<"done" | "skipped">;
}

/** Requests in flight at once: enough to be quick, few enough not to crowd the server. */
export const CONCURRENCY = 4;

function outcomeOf(error: unknown): BulkOutcome {
  if (error instanceof ApiError && error.status === 403)
    return { kind: "refused" };
  return { kind: "failed", error: describeError(error) };
}

/** Runs `action` on each item, at most `concurrency` at once; each succeeds or fails on its own. */
export async function runBulk<T>(
  items: readonly T[],
  key: (item: T) => string,
  action: Pick<BulkAction<T>, "allowed" | "act">,
  concurrency = CONCURRENCY,
): Promise<Map<string, BulkOutcome>> {
  const outcomes = new Map<string, BulkOutcome>();
  const queue = [...items];
  async function worker() {
    for (let item = queue.shift(); item !== undefined; item = queue.shift()) {
      if (action.allowed && !action.allowed(item)) {
        outcomes.set(key(item), { kind: "refused" });
        continue;
      }
      try {
        outcomes.set(key(item), { kind: await action.act(item) });
      } catch (e) {
        outcomes.set(key(item), outcomeOf(e));
      }
    }
  }
  await Promise.all(
    Array.from({ length: Math.min(concurrency, items.length) }, worker),
  );
  return outcomes;
}

/** Names shown after a refusal or a failure; the rest are counted. */
const NAMED = 3;

function names(list: string[]): string {
  if (list.length <= NAMED) return list.join(", ");
  return t("ui.bulk.andMore", {
    names: list.slice(0, NAMED).join(", "),
    count: list.length - NAMED,
  });
}

export interface BulkSummary {
  text: string;
  /** `ok` when every item was done or had nothing to do. */
  tone: "ok" | "warn";
}

/** "4 acknowledged, 1 refused: insufficient rights (jellyfin)", in the order of `items`. */
export function summarize<T>(
  items: readonly T[],
  key: (item: T) => string,
  name: (item: T) => string,
  outcomes: Map<string, BulkOutcome>,
  action: Pick<BulkAction<T>, "done" | "skipped">,
): BulkSummary {
  let done = 0;
  let skipped = 0;
  const refused: string[] = [];
  const failed = new Map<string, string[]>();
  for (const item of items) {
    const outcome = outcomes.get(key(item));
    if (!outcome) continue;
    if (outcome.kind === "done") done += 1;
    else if (outcome.kind === "skipped") skipped += 1;
    else if (outcome.kind === "refused") refused.push(name(item));
    else
      failed.set(outcome.error, [
        ...(failed.get(outcome.error) ?? []),
        name(item),
      ]);
  }
  const parts: string[] = [];
  if (done > 0) parts.push(action.done(done));
  if (skipped > 0)
    parts.push(
      action.skipped
        ? action.skipped(skipped)
        : t("ui.bulk.skipped", { count: skipped }),
    );
  if (refused.length > 0)
    parts.push(
      `${t("ui.bulk.refused", { count: refused.length })} (${names(refused)})`,
    );
  for (const [error, of] of failed)
    parts.push(
      `${t("ui.bulk.failed", { count: of.length, error })} (${names(of)})`,
    );
  return {
    text: parts.join(", "),
    tone: refused.length > 0 || failed.size > 0 ? "warn" : "ok",
  };
}

/** The keys whose outcome was a refusal or a failure, to keep them selected. */
export function unsettled(outcomes: Map<string, BulkOutcome>): Set<string> {
  return new Set(
    [...outcomes]
      .filter(([, o]) => o.kind === "refused" || o.kind === "failed")
      .map(([k]) => k),
  );
}

/**
 * The checked rows of a table, by key. Rows a filter hides stay out of
 * `chosen`, so an action only ever reaches what is on screen.
 */
export function useSelection() {
  const [selected, setSelected] = useState<ReadonlySet<string>>(
    () => new Set(),
  );
  const change = useCallback((keys: readonly string[], on: boolean) => {
    setSelected((previous) => {
      const next = new Set(previous);
      for (const key of keys) {
        if (on) next.add(key);
        else next.delete(key);
      }
      return next;
    });
  }, []);
  const clear = useCallback(() => setSelected(new Set()), []);
  return { selected, change, clear, replace: setSelected };
}

export type Selection = ReturnType<typeof useSelection>;

/**
 * Runs bulk actions over `chosen` rows and keeps the last summary. Once
 * one finishes, only the rows refused or failed stay selected, and
 * `onDone` reloads the page's data.
 */
export function useBulkRunner<T>(
  selection: Selection,
  key: (item: T) => string,
  name: (item: T) => string,
  onDone: () => void,
) {
  const [busy, setBusy] = useState(false);
  const [summary, setSummary] = useState<BulkSummary | null>(null);
  async function run(items: readonly T[], action: BulkAction<T>) {
    setBusy(true);
    setSummary(null);
    try {
      const outcomes = await runBulk(items, key, action);
      setSummary(summarize(items, key, name, outcomes, action));
      selection.replace(unsettled(outcomes));
      onDone();
    } finally {
      setBusy(false);
    }
  }
  return { busy, summary, run, dismiss: () => setSummary(null) };
}
