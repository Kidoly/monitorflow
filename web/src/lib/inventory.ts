/**
 * The hardware and software inventory (ADR-0187): what each server is made
 * of and what it runs, its changes, the warranty and the fleet search.
 */

/** Item kinds, in the order the Inventory tab shows them. */
export const ITEM_KINDS = ["system", "processor", "memory", "disk", "nic", "software", "service", "feature", "licence"] as const;
export type ItemKind = (typeof ITEM_KINDS)[number];

/** What the fleet search looks for: software (every source) or one item kind. */
export const SEARCH_KINDS = ["software", "disk", "system", "processor", "memory", "nic", "service", "feature", "licence"] as const;
export type SearchKind = (typeof SEARCH_KINDS)[number];

export interface InventoryItem {
  kind: string;
  key: string;
  fields: Record<string, string>;
}

export interface Warranty {
  ends_on: string | null;
  note: string | null;
  updated_at: string;
}

export interface ServerInventory {
  reported: { collected_at: string; first_at: string; changed_at: string; checked_at: string } | null;
  items: InventoryItem[];
  package_count: number;
  warranty: Warranty | null;
  asset: { hostname: string; manufacturer: string | null; model: string | null; serial: string | null };
}

export interface InventoryChange {
  id: number;
  at: string;
  /** An item kind, or `package`. */
  kind: string;
  key: string;
  change: "added" | "removed" | "changed";
  before: Record<string, string> | null;
  after: Record<string, string> | null;
}

export interface InventoryChangeList {
  changes: InventoryChange[];
  next_cursor: string | null;
}

export interface InventoryHit {
  server_id: string;
  hostname: string;
  site_id: string;
  site: string;
  source: string;
  name: string;
  version: string | null;
  fields: Record<string, string>;
}

export interface InventorySearch {
  hits: InventoryHit[];
  truncated: boolean;
}

export interface InventoryQuery {
  kind: SearchKind;
  q: string;
  exact: boolean;
  below: string;
  atLeast: string;
}

export const EMPTY_QUERY: InventoryQuery = { kind: "software", q: "", exact: false, below: "", atLeast: "" };

/** The query string of a search, without `?`; `format` for the CSV file, `serverId` for one server's. */
export function inventoryQueryString(query: InventoryQuery, format?: "csv", serverId?: string): string {
  const params = new URLSearchParams({ kind: query.kind });
  if (serverId) params.set("server_id", serverId);
  if (query.q.trim()) params.set("q", query.q.trim());
  if (query.exact) params.set("exact", "true");
  if (query.below.trim()) params.set("version_below", query.below.trim());
  if (query.atLeast.trim()) params.set("version_at_least", query.atLeast.trim());
  if (format) params.set("format", format);
  return params.toString();
}

/** Link to the CSV file of a search, of one server's items when `serverId` is given. */
export function inventoryCsvHref(query: InventoryQuery, serverId?: string): string {
  return `/api/inventory?${inventoryQueryString(query, "csv", serverId)}`;
}

/** Software needs two characters to search; the other kinds can list everything. */
export function canSearch(query: InventoryQuery): boolean {
  return query.kind !== "software" || query.q.trim().length >= 2;
}

/** The items of `kind`. */
export function itemsOf(items: InventoryItem[], kind: ItemKind): InventoryItem[] {
  return items.filter((i) => i.kind === kind);
}

/** A byte count field as a number, or null when absent or not a positive number. */
export function byteCount(value: string | undefined): number | null {
  const bytes = Number(value);
  return value && Number.isFinite(bytes) && bytes > 0 ? bytes : null;
}

/** The fields an item's change touched, as "model: A → B". */
export function changedFields(change: InventoryChange): string[] {
  const before = change.before ?? {};
  const after = change.after ?? {};
  const names = [...new Set([...Object.keys(before), ...Object.keys(after)])].sort();
  return names
    .filter((name) => before[name] !== after[name])
    .map((name) => `${name}: ${before[name] ?? "∅"} → ${after[name] ?? "∅"}`);
}
