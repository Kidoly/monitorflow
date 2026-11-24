"use client";

import { type FormEvent, useCallback, useState } from "react";

import { type PlainKey, t } from "@/i18n";
import { api, describeError } from "@/lib/api";
import {
  type InventoryHit,
  type InventoryQuery,
  type InventorySearch as Result,
  SEARCH_KINDS,
  type SearchKind,
  canSearch,
  inventoryCsvHref,
} from "@/lib/inventory";
import { serverHref } from "@/lib/routes";
import { useUrlParams } from "@/lib/urlState";
import { usePolledData } from "@/lib/usePolledData";
import { Button, buttonClass, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { missingText } from "@/components/ui/Missing";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, LoadingBlock, PageHeader } from "@/components/ui/PageState";
import { Switch } from "@/components/ui/Switch";
import { TableBox } from "@/components/ui/TableBox";

import styles from "./InventoryTab.module.css";

/** The search the URL holds: `kind`, `q`, `exact`, `below`, `from`. */
export function queryFromParams(params: URLSearchParams): InventoryQuery {
  const kind = SEARCH_KINDS.find((k) => k === params.get("kind")) ?? "software";
  return {
    kind,
    q: params.get("q") ?? "",
    exact: params.get("exact") === "1",
    below: params.get("below") ?? "",
    atLeast: params.get("from") ?? "",
  };
}

/** Where a hit comes from, in words. */
function source(hit: InventoryHit): string {
  const key = `inventory.source.${hit.source}` as PlainKey;
  const text = t(key);
  if (text !== key) return text;
  const kind = `inventory.kind.${hit.source}` as PlainKey;
  return t(kind) === kind ? hit.source : t(kind);
}

const details = (hit: InventoryHit) =>
  Object.entries(hit.fields)
    .filter(([name]) => name !== "version")
    .map(([name, value]) => `${name}=${value}`)
    .join(" · ");

const columns = (): Column<InventoryHit>[] => [
  {
    key: "server",
    header: t("inventory.column.server"),
    sortValue: (h) => h.hostname,
    cell: (h) => <PrimaryCell title={h.hostname} href={serverHref(h.server_id, "inventory")} sub={h.site} />,
  },
  { key: "source", header: t("inventory.column.source"), sortValue: source, cell: source },
  { key: "name", header: t("inventory.column.name"), sortValue: (h) => h.name.toLowerCase(), cell: (h) => h.name },
  {
    key: "version",
    header: t("inventory.column.version"),
    sortValue: (h) => h.version ?? "",
    cell: (h) => (h.version ? <span className={styles.mono}>{h.version}</span> : missingText("none")),
  },
  { key: "details", header: t("inventory.column.details"), cell: (h) => <span className={styles.details}>{details(h)}</span> },
];

/** The fleet's inventory search (ADR-0187): software by name and version, hardware by any field, and its CSV. */
export function InventorySearchPage() {
  const [params, setParams] = useUrlParams();
  const committed = queryFromParams(params);
  const key = params.toString();
  const [draft, setDraft] = useState<InventoryQuery>(committed);
  const ready = canSearch(committed) && params.has("kind");
  // A search runs when the URL changes; the results are not polled.
  const fetcher = useCallback(
    (signal: AbortSignal) => api.searchInventory(queryFromParams(new URLSearchParams(key)), signal),
    [key],
  );
  const polled = usePolledData(fetcher, { intervalMs: 24 * 3600_000, enabled: ready });
  const result: Result | null = polled.data ?? null;
  const error = polled.error ? describeError(polled.error) : null;
  const loading = ready && !polled.data && !polled.error;

  function submit(e: FormEvent) {
    e.preventDefault();
    setParams({
      kind: draft.kind,
      q: draft.q.trim() || null,
      exact: draft.exact ? "1" : null,
      below: draft.below.trim() || null,
      from: draft.atLeast.trim() || null,
    });
  }

  const set = <K extends keyof InventoryQuery>(name: K, value: InventoryQuery[K]) => setDraft((d) => ({ ...d, [name]: value }));
  const software = draft.kind === "software";
  const title = t("inventory.page.title");
  return (
    <Page>
      <PageHeader title={title} sub={t("inventory.page.intro")} />
      <form className={styles.row} onSubmit={submit}>
        <label className={styles.field}>
          <span>{t("inventory.search.kind")}</span>
          <select className={fieldClass} value={draft.kind} onChange={(e) => set("kind", e.target.value as SearchKind)}>
            {SEARCH_KINDS.map((kind) => (
              <option key={kind} value={kind}>
                {t(`inventory.search.kind.${kind}`)}
              </option>
            ))}
          </select>
        </label>
        <label className={`${styles.field} ${styles.grow}`}>
          <span>{software ? t("inventory.search.q") : t("inventory.search.qAny")}</span>
          <input
            className={fieldClass}
            type="search"
            value={draft.q}
            maxLength={200}
            placeholder={software ? t("inventory.search.qPlaceholder") : undefined}
            onChange={(e) => set("q", e.target.value)}
          />
        </label>
        <label className={styles.field}>
          <span>{t("inventory.search.below")}</span>
          <input className={fieldClass} value={draft.below} maxLength={64} onChange={(e) => set("below", e.target.value)} />
        </label>
        <label className={styles.field}>
          <span>{t("inventory.search.atLeast")}</span>
          <input className={fieldClass} value={draft.atLeast} maxLength={64} onChange={(e) => set("atLeast", e.target.value)} />
        </label>
        <Switch checked={draft.exact} onChange={(on) => set("exact", on)} label={t("inventory.search.exact")} />
        <Button variant="k" type="submit" disabled={!canSearch(draft)}>
          {t("inventory.search.submit")}
        </Button>
        {ready && (
          <a className={buttonClass()} href={inventoryCsvHref(committed)} download>
            {t("inventory.exportCsv")}
          </a>
        )}
      </form>
      {software && draft.q.trim().length < 2 && <p className={styles.hint}>{t("inventory.search.tooShort")}</p>}
      {error && <Banner tone="bad">{t("ui.page.loadFailed", { what: t("inventory.search.what") })} {error}</Banner>}
      {!ready ? (
        <EmptyState>{t("inventory.search.start")}</EmptyState>
      ) : loading ? (
        <LoadingBlock />
      ) : result && result.hits.length === 0 ? (
        <EmptyState>{t("inventory.search.none")}</EmptyState>
      ) : result ? (
        <TableBox title={title} meta={t("inventory.search.count", { count: result.hits.length })}>
          {result.truncated && <Banner tone="warn">{t("inventory.search.truncated", { count: result.hits.length })}</Banner>}
          <DataTable key={key} dense bare label={title} columns={columns()} rows={result.hits} rowKey={(h) => `${h.server_id}:${h.source}:${h.name}:${h.fields.arch ?? ""}:${h.fields.scope ?? ""}`} pageSize={50} />
        </TableBox>
      ) : null}
    </Page>
  );
}

