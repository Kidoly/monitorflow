"use client";

import { type ReactNode, useCallback, useEffect, useState } from "react";

import { type PlainKey, t } from "@/i18n";
import { type Package, type ServerDetail, api, describeError, isAbortError } from "@/lib/api";
import { formatBytes } from "@/lib/format";
import {
  EMPTY_QUERY,
  type InventoryChange,
  type InventoryItem,
  type ItemKind,
  type ServerInventory,
  byteCount,
  changedFields,
  inventoryCsvHref,
  itemsOf,
} from "@/lib/inventory";
import { modifiesSite, useMe } from "@/lib/me";
import { usePolledData } from "@/lib/usePolledData";
import { Button, buttonClass, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { type Fact, FactsBox } from "@/components/ui/FactsBox";
import { missingText } from "@/components/ui/Missing";
import { Page } from "@/components/ui/Page";
import { Banner, EmptyState, ErrorState, LoadingBlock } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label } from "@/components/ui/StatusLabel";
import { TableBox } from "@/components/ui/TableBox";

import styles from "./InventoryTab.module.css";

/** The inventory changes slowly: a look every 5 minutes is plenty. */
const REFRESH_MS = 5 * 60_000;
/** Rows of each table before "Load more". */
const PAGE = 20;

/** A word for a value of the catalog, or the value itself when the catalog has none. */
function word(prefix: string, value: string | undefined): string {
  if (!value) return missingText("unknown");
  const key = `inventory.${prefix}.${value}` as PlainKey;
  const text = t(key);
  return text === key ? value : text;
}

const text = (value: string | undefined): ReactNode => value || missingText("unknown");

/** A table of the items of one kind, with the fields of `columns`. */
function ItemTable({
  title,
  items,
  columns,
}: {
  title: string;
  items: InventoryItem[];
  columns: Column<InventoryItem>[];
}) {
  if (items.length === 0) return null;
  return (
    <TableBox title={title}>
      <DataTable dense bare label={title} columns={columns} rows={items} rowKey={(i) => i.key} pageSize={PAGE} />
    </TableBox>
  );
}

const field =
  (header: PlainKey, name: string, format?: (value: string | undefined) => ReactNode): Column<InventoryItem> => ({
    key: name,
    header: t(header),
    sortValue: (i) => i.fields[name] ?? "",
    cell: (i) => (format ? format(i.fields[name]) : text(i.fields[name])),
  });

const keyColumn = (header: PlainKey): Column<InventoryItem> => ({
  key: "key",
  header: t(header),
  sortValue: (i) => i.key,
  cell: (i) => <span className={styles.mono}>{i.key}</span>,
});

const size = (value: string | undefined) => {
  const bytes = byteCount(value);
  return bytes === null ? missingText("unknown") : formatBytes(bytes);
};

const speed = (key: "speedMbps" | "speedMhz") => (value: string | undefined) =>
  value ? t(`inventory.${key}`, { speed: value }) : missingText("unknown");

/** The Hardware box: the system item, the processors and the report's dates. */
function Hardware({ inventory, now }: { inventory: ServerInventory; now: Date }) {
  const system = itemsOf(inventory.items, "system")[0]?.fields ?? {};
  const processors = itemsOf(inventory.items, "processor");
  const first = processors[0]?.fields;
  const sameModel = processors.every((p) => p.fields.model === first?.model);
  const processor = !first
    ? missingText("unknown")
    : processors.length > 1 && sameModel
      ? t("inventory.fact.processors", { count: processors.length, model: first.model ?? "" })
      : processors.map((p) => p.fields.model).join(", ");
  const cores = processors.reduce((n, p) => n + Number(p.fields.cores ?? 0), 0);
  const threads = processors.reduce((n, p) => n + Number(p.fields.threads ?? 0), 0);
  const memory = byteCount(system.memory_bytes);
  const bios = system.bios_version
    ? system.bios_date
      ? t("inventory.fact.biosValue", { version: system.bios_version, date: system.bios_date })
      : system.bios_version
    : missingText("unknown");
  const board = [system.board_manufacturer, system.board_model].filter(Boolean).join(" ");
  const facts: Fact[] = [
    { label: t("inventory.fact.manufacturer"), value: text(system.manufacturer) },
    { label: t("inventory.fact.model"), value: text(system.model) },
    { label: t("inventory.fact.serial"), value: text(system.serial ?? system.chassis_serial), mono: Boolean(system.serial) },
    { label: t("inventory.fact.bios"), value: bios },
    {
      label: t("inventory.fact.processor"),
      value: first ? t("inventory.fact.processorValue", { model: processor, cores, threads }) : processor,
    },
    { label: t("inventory.fact.memory"), value: memory === null ? missingText("unknown") : formatBytes(memory) },
    { label: t("inventory.fact.os"), value: text(system.os) },
    { label: t("inventory.fact.kernel"), value: text(system.kernel ?? system.os_version), mono: true },
  ];
  const details: Fact[] = [
    { label: t("inventory.fact.chassis"), value: text(system.chassis) },
    { label: t("inventory.fact.board"), value: board || missingText("unknown") },
    { label: t("inventory.fact.architecture"), value: text(system.architecture) },
    ...(inventory.reported
      ? [
          { label: t("inventory.fact.collected"), value: <RelativeTime value={inventory.reported.collected_at} now={now} /> },
          { label: t("inventory.fact.changed"), value: <RelativeTime value={inventory.reported.changed_at} now={now} /> },
          { label: t("inventory.fact.checked"), value: <RelativeTime value={inventory.reported.checked_at} now={now} /> },
        ]
      : []),
  ];
  return <FactsBox title={t("inventory.hardware.title")} columns={2} facts={facts} details={details} />;
}

/** The warranty, typed by people (ADR-0187), with its form for operators of the site. */
function Warranty({
  server,
  inventory,
  now,
  onSaved,
}: {
  server: ServerDetail;
  inventory: ServerInventory;
  now: Date;
  onSaved: () => void;
}) {
  const me = useMe();
  const canEdit = modifiesSite(me, server.customer_id, server.site_id);
  const warranty = inventory.warranty;
  const [editing, setEditing] = useState(false);
  const [endsOn, setEndsOn] = useState("");
  const [note, setNote] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  function open() {
    setEndsOn(warranty?.ends_on ?? "");
    setNote(warranty?.note ?? "");
    setError(null);
    setEditing(true);
  }

  async function save(clear: boolean) {
    setSaving(true);
    setError(null);
    try {
      await api.setWarranty(server.id, clear ? { ends_on: null, note: null } : { ends_on: endsOn || null, note: note.trim() || null });
      setEditing(false);
      onSaved();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setSaving(false);
    }
  }

  const today = now.toISOString().slice(0, 10);
  const soon = new Date(now.getTime() + 90 * 86_400_000).toISOString().slice(0, 10);
  const ends = warranty?.ends_on;
  const endsValue = !ends ? (
    missingText("notSet")
  ) : ends < today ? (
    <Label tone="bad">{t("inventory.warranty.expired", { date: ends })}</Label>
  ) : ends <= soon ? (
    <Label tone="warn">{t("inventory.warranty.soon", { date: ends })}</Label>
  ) : (
    t("inventory.warranty.until", { date: ends })
  );
  return (
    <FactsBox
      title={t("inventory.warranty.title")}
      columns={2}
      actions={
        canEdit && !editing ? (
          <Button size="small" onClick={open}>
            {warranty ? t("inventory.warranty.edit") : t("inventory.warranty.set")}
          </Button>
        ) : undefined
      }
      facts={[
        { label: t("inventory.warranty.ends"), value: endsValue },
        { label: t("inventory.warranty.note"), value: warranty?.note ?? missingText("none") },
      ]}
    >
      {editing && (
        <form
          className={styles.form}
          onSubmit={(e) => {
            e.preventDefault();
            void save(false);
          }}
        >
          <p className={styles.hint}>{t("inventory.warranty.hint")}</p>
          <div className={styles.row}>
            <label className={styles.field}>
              <span>{t("inventory.warranty.ends")}</span>
              <input className={fieldClass} type="date" value={endsOn} onChange={(e) => setEndsOn(e.target.value)} />
            </label>
            <label className={`${styles.field} ${styles.grow}`}>
              <span>{t("inventory.warranty.note")}</span>
              <input
                className={fieldClass}
                value={note}
                maxLength={500}
                placeholder={t("inventory.warranty.notePlaceholder")}
                onChange={(e) => setNote(e.target.value)}
              />
            </label>
            <Button variant="k" type="submit" disabled={saving}>
              {saving ? t("inventory.warranty.saving") : t("inventory.warranty.save")}
            </Button>
            {warranty && (
              <Button variant="danger" disabled={saving} onClick={() => void save(true)}>
                {t("inventory.warranty.remove")}
              </Button>
            )}
            <Button variant="ghost" disabled={saving} onClick={() => setEditing(false)}>
              {t("inventory.warranty.cancel")}
            </Button>
          </div>
          {error && <Banner tone="bad">{t("inventory.warranty.failed", { error })}</Banner>}
        </form>
      )}
    </FactsBox>
  );
}

interface SoftwareRow {
  key: string;
  name: string;
  sub: string;
  version: string;
  publisher: string;
}

const softwareColumns = (): Column<SoftwareRow>[] => [
  {
    key: "name",
    header: t("inventory.column.name"),
    sortValue: (r) => r.name.toLowerCase(),
    cell: (r) => <PrimaryCell title={r.name} sub={r.sub || undefined} />,
  },
  { key: "version", header: t("inventory.column.version"), sortValue: (r) => r.version, cell: (r) => <span className={styles.mono}>{r.version}</span> },
  { key: "publisher", header: t("inventory.column.publisher"), sortValue: (r) => r.publisher, cell: (r) => r.publisher || missingText("unknown") },
];

/**
 * Installed software: the packages of a Linux server, from its package
 * list, or the programs of a Windows server, from its inventory.
 */
function Software({ server, inventory }: { server: ServerDetail; inventory: ServerInventory }) {
  const windows = server.os_family === "windows";
  const [packages, setPackages] = useState<Package[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  useEffect(() => {
    if (windows) return;
    const controller = new AbortController();
    api
      .serverPackages(server.id, controller.signal)
      .then((list) => setPackages(list.packages.filter((p) => p.version !== "")))
      .catch((e: unknown) => {
        if (!isAbortError(e)) setError(describeError(e));
      });
    return () => controller.abort();
  }, [server.id, windows]);

  const rows: SoftwareRow[] = windows
    ? itemsOf(inventory.items, "software").map((i) => ({
        key: i.key,
        name: i.key,
        sub: i.fields.installed_on ?? "",
        version: i.fields.version ?? "",
        publisher: i.fields.publisher ?? "",
      }))
    : (packages ?? []).map((p) => ({ key: `${p.name}:${p.arch}`, name: p.name, sub: p.arch, version: p.version, publisher: "" }));
  const needle = filter.trim().toLowerCase();
  const shown = needle ? rows.filter((r) => r.name.toLowerCase().includes(needle)) : rows;
  const title = t("inventory.software.title");
  const csv = inventoryCsvHref(EMPTY_QUERY, server.id);
  const columns = softwareColumns().filter((c) => windows || c.key !== "publisher");
  return (
    <TableBox title={title} meta={t("inventory.software.count", { count: rows.length })}>
      <div className={styles.toolbar}>
        <label className={styles.field}>
          <span>{t("inventory.software.filter")}</span>
          <input
            className={fieldClass}
            type="search"
            value={filter}
            placeholder={t("inventory.software.filterPlaceholder")}
            onChange={(e) => setFilter(e.target.value)}
          />
        </label>
        <a className={buttonClass()} href={csv} download>
          {t("inventory.exportCsv")}
        </a>
      </div>
      {error ? (
        <Banner tone="bad">{error}</Banner>
      ) : !windows && packages === null ? (
        <LoadingBlock label={t("inventory.software.loading")} />
      ) : shown.length === 0 ? (
        <EmptyState>{t("inventory.software.none")}</EmptyState>
      ) : (
        <DataTable key={needle} dense bare label={title} columns={columns} rows={shown} rowKey={(r) => r.key} pageSize={PAGE} />
      )}
    </TableBox>
  );
}

const changeTone = { added: "ok", removed: "bad", changed: "warn" } as const;

/** What changed, newest first, a page at a time. */
function Changes({ server, now }: { server: ServerDetail; now: Date }) {
  const fetcher = useCallback((signal: AbortSignal) => api.inventoryChanges(server.id, undefined, signal), [server.id]);
  const first = usePolledData(fetcher, { intervalMs: REFRESH_MS });
  // Pages after the first, added by "Load more".
  const [more, setMore] = useState<{ changes: InventoryChange[]; cursor: string | null } | null>(null);
  const [moreError, setMoreError] = useState<string | null>(null);
  const changes = first.data ? [...first.data.changes, ...(more?.changes ?? [])] : null;
  const cursor = more ? more.cursor : (first.data?.next_cursor ?? null);
  const error = first.data ? moreError : first.error ? describeError(first.error) : null;

  async function loadMore() {
    if (!cursor) return;
    try {
      const page = await api.inventoryChanges(server.id, cursor);
      setMore((old) => ({ changes: [...(old?.changes ?? []), ...page.changes], cursor: page.next_cursor }));
      setMoreError(null);
    } catch (e) {
      setMoreError(describeError(e));
    }
  }

  const title = t("inventory.changes.title");
  const columns: Column<InventoryChange>[] = [
    { key: "at", header: t("inventory.changes.when"), cell: (c) => <RelativeTime value={c.at} now={now} /> },
    {
      key: "item",
      header: t("inventory.changes.item"),
      cell: (c) => <PrimaryCell title={c.key} sub={word("kind", c.kind)} />,
    },
    {
      key: "change",
      header: t("inventory.changes.change"),
      cell: (c) => <Label tone={changeTone[c.change]}>{t(`inventory.changes.${c.change}`)}</Label>,
    },
    {
      key: "details",
      header: t("inventory.changes.details"),
      cell: (c) => (
        <span className={styles.details}>
          {c.change === "changed" ? changedFields(c).join(" · ") : (c.after ?? c.before)?.version ?? ""}
        </span>
      ),
    },
  ];
  return (
    <TableBox title={title}>
      {error && <Banner tone="bad">{error}</Banner>}
      {changes === null ? (
        <LoadingBlock label={t("inventory.changes.loading")} />
      ) : changes.length === 0 ? (
        <EmptyState>{t("inventory.changes.none")}</EmptyState>
      ) : (
        <DataTable
          dense
          bare
          label={title}
          columns={columns}
          rows={changes}
          rowKey={(c) => String(c.id)}
          hasMore={cursor !== null}
          onLoadMore={() => void loadMore()}
        />
      )}
    </TableBox>
  );
}

const tables = (): { kind: ItemKind; title: PlainKey; columns: Column<InventoryItem>[] }[] => [
  {
    kind: "disk",
    title: "inventory.disks.title",
    columns: [
      keyColumn("inventory.column.device"),
      field("inventory.column.model", "model"),
      field("inventory.column.serial", "serial"),
      field("inventory.column.size", "size_bytes", size),
      field("inventory.column.media", "media", (v) => (v ? word("media", v) : missingText("unknown"))),
      field("inventory.column.interface", "transport"),
      field("inventory.column.firmware", "firmware"),
    ],
  },
  {
    kind: "memory",
    title: "inventory.memory.title",
    columns: [
      keyColumn("inventory.column.slot"),
      field("inventory.column.size", "size_bytes", size),
      field("inventory.column.speed", "speed_mhz", speed("speedMhz")),
      field("inventory.column.maker", "manufacturer"),
      field("inventory.column.part", "part_number"),
      field("inventory.column.serial", "serial"),
    ],
  },
  {
    kind: "nic",
    title: "inventory.nics.title",
    columns: [
      keyColumn("inventory.column.name"),
      field("inventory.column.model", "model", (v) => v || missingText("unknown")),
      field("inventory.column.mac", "mac"),
      field("inventory.column.speed", "speed_mbps", speed("speedMbps")),
      field("inventory.column.addresses", "addresses", (v) => v || missingText("none")),
      field("inventory.column.driver", "driver"),
    ],
  },
  {
    kind: "service",
    title: "inventory.services.title",
    columns: [
      keyColumn("inventory.column.name"),
      field("inventory.column.start", "start", (v) => word("start", v)),
      field("inventory.column.product", "display_name", (v) => v || missingText("none")),
    ],
  },
  {
    kind: "feature",
    title: "inventory.features.title",
    columns: [keyColumn("inventory.column.name"), field("inventory.column.product", "display_name")],
  },
  {
    kind: "licence",
    title: "inventory.licences.title",
    columns: [
      keyColumn("inventory.column.product"),
      field("inventory.column.status", "status", (v) => word("licence", v)),
      field("inventory.column.key", "partial_key"),
    ],
  },
];

/** The Inventory tab of a server (ADR-0187). */
export function InventoryTab({ server }: { server: ServerDetail }) {
  const fetcher = useCallback((signal: AbortSignal) => api.serverInventory(server.id, signal), [server.id]);
  const polled = usePolledData(fetcher, { intervalMs: REFRESH_MS });
  const inventory = polled.data;
  if (!inventory) {
    return polled.error ? (
      <ErrorState what={t("inventory.what")} error={describeError(polled.error)} onRetry={polled.refresh} />
    ) : (
      <LoadingBlock label={t("inventory.loading")} />
    );
  }
  const now = polled.updatedAt ?? new Date();
  const [services, rest] = [tables().filter((x) => x.kind === "service"), tables().filter((x) => x.kind !== "service")];
  const table = (x: ReturnType<typeof tables>[number]) => (
    <ItemTable key={x.kind} title={t(x.title)} items={itemsOf(inventory.items, x.kind)} columns={x.columns} />
  );
  return (
    <Page>
      {inventory.reported === null ? (
        <EmptyState title={t("inventory.notReported")}>{t("inventory.notReportedText", { server: server.hostname })}</EmptyState>
      ) : (
        <Hardware inventory={inventory} now={now} />
      )}
      <Warranty server={server} inventory={inventory} now={now} onSaved={polled.refresh} />
      {inventory.reported !== null && (
        <>
          {rest.filter((x) => x.kind !== "feature" && x.kind !== "licence").map(table)}
          <Software server={server} inventory={inventory} />
          {services.map(table)}
          {rest.filter((x) => x.kind === "feature" || x.kind === "licence").map(table)}
        </>
      )}
      <Changes server={server} now={now} />
    </Page>
  );
}
