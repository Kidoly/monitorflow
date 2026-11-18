"use client";

import Link from "next/link";
import { useCallback, useEffect, useId, useMemo, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type ImportItem,
  type ImportList,
  type ImportRun,
  type ImportSource,
  type Site,
  api,
  describeError,
} from "@/lib/api";
import {
  LISTS,
  countsText,
  createdAnything,
  grouped,
  itemTitle,
  kindHeading,
  leftOut,
  noteText,
  originKind,
  tickable,
  toCreate,
} from "@/lib/imports";
import { formatDateTime } from "@/lib/format";
import { isAdministrator, multiSite, useMe } from "@/lib/me";
import { browserTimeZone, timeZones } from "@/lib/onCall";
import { TOKENS_PATH } from "@/lib/routes";
import { useUrlParams } from "@/lib/urlState";

import { Button, fieldClass } from "../ui/Button";
import { cx } from "../ui/cx";
import { InlineConfirm } from "../ui/InlineConfirm";
import { Page } from "../ui/Page";
import {
  Banner,
  ErrorState,
  LoadingBlock,
  NoPermission,
  PageHeader,
} from "../ui/PageState";
import { Label } from "../ui/StatusLabel";
import type { Tone } from "../ui/tone";
import styles from "./Imports.module.css";

const LIST_TONES: Record<ImportList, Tone> = {
  exact: "ok",
  approximated: "warn",
  not_movable: "neutral",
};

/** `/settings/import/`: moving from another monitoring tool (ADR-0146). Administrators only. */
export function ImportPage() {
  const me = useMe();
  if (!isAdministrator(me)) {
    return (
      <NoPermission title={t("imports.noPermission")}>
        {t("imports.noPermissionNote")}
      </NoPermission>
    );
  }
  return <Imports />;
}

function Imports() {
  const [params, setParams] = useUrlParams();
  const id = params.get("id");
  return (
    <Page>
      <PageHeader title={t("imports.title")} />
      <p className={styles.intro}>{t("imports.intro")}</p>
      {id ? (
        <RunView id={id} onClose={() => setParams({ id: null })} />
      ) : (
        <>
          <StartForm onPreview={(run) => setParams({ id: run.id })} />
          <History onOpen={(run) => setParams({ id: run.id })} />
        </>
      )}
    </Page>
  );
}

/** Reads a file as text, or as base64 when it is not UTF-8. */
async function fileBody(
  file: File,
): Promise<{ name: string; content: string; base64?: boolean }> {
  const bytes = new Uint8Array(await file.arrayBuffer());
  try {
    return {
      name: file.name,
      content: new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    };
  } catch {
    let binary = "";
    for (const byte of bytes) binary += String.fromCharCode(byte);
    return { name: file.name, content: btoa(binary), base64: true };
  }
}

/** The label of a connection field, from the catalog, or its key. */
function fieldLabel(key: string): string {
  const text = (t as (key: string) => string)(`imports.field.${key}`);
  return text === `imports.field.${key}` ? key : text;
}

function StartForm({ onPreview }: { onPreview: (run: ImportRun) => void }) {
  const me = useMe();
  const formId = useId();
  const [sources, setSources] = useState<ImportSource[] | null>(null);
  const [sites, setSites] = useState<Site[]>([]);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [sourceId, setSourceId] = useState("");
  const [siteId, setSiteId] = useState("");
  const [zone, setZone] = useState(browserTimeZone);
  const [mode, setMode] = useState<"files" | "connect">("files");
  const [files, setFiles] = useState<File[]>([]);
  const [fields, setFields] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    Promise.all([
      api.importSources(controller.signal),
      api.sites(controller.signal),
    ])
      .then(([s, list]) => {
        setSources(s.sources);
        setSites(list.sites);
        const first = s.sources[0];
        if (first) setSourceId((current) => current || first.id);
        const firstSite = list.sites[0];
        if (firstSite) setSiteId((current) => current || firstSite.id);
      })
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setLoadError(e);
      });
    return () => controller.abort();
  }, []);

  const source = sources?.find((s) => s.id === sourceId) ?? null;
  // A source that reads no files only connects, and the other way round.
  const shownMode =
    source && source.extensions.length === 0
      ? "connect"
      : source && source.connect.length === 0
        ? "files"
        : mode;

  if (loadError)
    return (
      <ErrorState
        what={t("imports.what.sources")}
        error={describeError(loadError)}
      />
    );
  if (!sources) return <LoadingBlock />;
  if (sources.length === 0)
    return <Banner tone="plain">{t("imports.noSources")}</Banner>;

  async function read() {
    if (!source) return;
    setBusy(true);
    setError(null);
    try {
      const body =
        shownMode === "files"
          ? { files: await Promise.all(files.map(fileBody)) }
          : {
              connection: Object.fromEntries(
                Object.entries(fields).filter(([, v]) => v.trim() !== ""),
              ),
            };
      const run = await api.previewImport({
        source: source.id,
        ...(multiSite(me) && siteId ? { site_id: siteId } : {}),
        time_zone: zone,
        ...body,
      });
      // The token is not kept once read.
      setFields((current) =>
        Object.fromEntries(
          Object.entries(current).filter(
            ([k]) => !source.connect.find((f) => f.key === k)?.secret,
          ),
        ),
      );
      onPreview(run);
    } catch (e) {
      setError(
        e instanceof ApiError && e.status < 500 ? e.message : describeError(e),
      );
    } finally {
      setBusy(false);
    }
  }

  const ready =
    shownMode === "files"
      ? files.length > 0
      : source?.connect.every(
          (f) => f.optional || (fields[f.key] ?? "").trim() !== "",
        );

  return (
    <section className={styles.box} aria-labelledby={`${formId}-title`}>
      <h2 id={`${formId}-title`} className={styles.title}>
        {t("imports.start.title")}
      </h2>
      <fieldset className={styles.fieldset}>
        <legend className={styles.legend}>{t("imports.start.source")}</legend>
        <div className={styles.sources}>
          {sources.map((s) => (
            <label
              key={s.id}
              className={cx(
                styles.source,
                s.id === sourceId && styles.sourceChosen,
              )}
            >
              <input
                type="radio"
                name={`${formId}-source`}
                value={s.id}
                checked={s.id === sourceId}
                onChange={() => setSourceId(s.id)}
              />
              {s.name}
            </label>
          ))}
        </div>
      </fieldset>
      {multiSite(me) && (
        <label className={styles.field}>
          <span>{t("imports.start.site")}</span>
          <select
            className={fieldClass}
            value={siteId}
            onChange={(e) => setSiteId(e.target.value)}
          >
            {sites.map((site) => (
              <option key={site.id} value={site.id}>
                {site.name}
              </option>
            ))}
          </select>
        </label>
      )}
      <label className={styles.field}>
        <span>{t("imports.start.zone")}</span>
        <select className={fieldClass} value={zone} onChange={(e) => setZone(e.target.value)}>
          {timeZones(zone).map((z) => (
            <option key={z} value={z}>
              {z.replaceAll("_", " ")}
            </option>
          ))}
        </select>
        <span className={styles.note}>{t("imports.start.zoneNote")}</span>
      </label>
      {source && source.extensions.length > 0 && source.connect.length > 0 && (
        <div
          className={styles.modes}
          role="radiogroup"
          aria-label={t("imports.start.how")}
        >
          {(["files", "connect"] as const).map((m) => (
            <label key={m} className={styles.check}>
              <input
                type="radio"
                name={`${formId}-mode`}
                checked={mode === m}
                onChange={() => setMode(m)}
              />{" "}
              {m === "files"
                ? t("imports.start.files")
                : t("imports.start.connect")}
            </label>
          ))}
        </div>
      )}
      {source && shownMode === "files" && (
        <label className={styles.field}>
          <span>
            {t("imports.start.chooseFiles")}{" "}
            <span className={styles.note}>
              {source.extensions.map((e) => `.${e}`).join(", ")}
              {source.many_files ? ` · ${t("imports.start.many")}` : ""}
            </span>
          </span>
          <input
            type="file"
            multiple={source.many_files}
            accept={source.extensions.map((e) => `.${e}`).join(",")}
            onChange={(e) => setFiles(Array.from(e.target.files ?? []))}
          />
        </label>
      )}
      {source && shownMode === "connect" && (
        <>
          {source.connect.map((field) => (
            <label key={field.key} className={styles.field}>
              <span>
                {fieldLabel(field.key)}{" "}
                {field.optional && (
                  <span className={styles.note}>
                    {t("imports.start.optional")}
                  </span>
                )}
              </span>
              <input
                className={cx(fieldClass, styles.mono)}
                type={field.secret ? "password" : "text"}
                autoComplete={field.secret ? "new-password" : "off"}
                spellCheck={false}
                placeholder={field.placeholder}
                value={fields[field.key] ?? ""}
                onChange={(e) =>
                  setFields((current) => ({
                    ...current,
                    [field.key]: e.target.value,
                  }))
                }
              />
            </label>
          ))}
          <p className={styles.note}>{t("imports.start.readOnly")}</p>
        </>
      )}
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
      <div className={styles.buttons}>
        <Button
          variant="k"
          disabled={!ready || busy}
          onClick={() => void read()}
        >
          {busy ? t("imports.start.reading") : t("imports.start.read")}
        </Button>
      </div>
    </section>
  );
}

function History({ onOpen }: { onOpen: (run: ImportRun) => void }) {
  const [runs, setRuns] = useState<ImportRun[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    const controller = new AbortController();
    api
      .imports(controller.signal)
      .then((list) => setRuns(list.imports))
      .catch((e: unknown) => {
        if (!controller.signal.aborted) setError(e);
      });
    return () => controller.abort();
  }, []);
  if (error)
    return (
      <ErrorState
        what={t("imports.what.history")}
        error={describeError(error)}
      />
    );
  if (!runs || runs.length === 0) return null;
  return (
    <section className={styles.box} aria-labelledby="imports-history">
      <h2 id="imports-history" className={styles.title}>
        {t("imports.history.title")}
      </h2>
      <ul className={styles.history}>
        {runs.map((run) => (
          <li key={run.id} className={styles.historyRow}>
            <span>
              <strong>{run.source_name}</strong> · {run.input} ·{" "}
              {formatDateTime(run.created_at)}
              {run.created_by ? ` · ${run.created_by}` : ""}
            </span>
            <StateLabel run={run} />
            <Button size="small" onClick={() => onOpen(run)}>
              {t("imports.history.open")}
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}

function StateLabel({ run }: { run: ImportRun }) {
  const tone: Tone =
    run.state === "confirmed"
      ? "ok"
      : run.state === "undone"
        ? "neutral"
        : "info";
  return (
    <Label tone={tone} small>
      {t(`imports.state.${run.state}`)}
    </Label>
  );
}

function RunView({ id, onClose }: { id: string; onClose: () => void }) {
  const [run, setRun] = useState<ImportRun | null>(null);
  const [error, setError] = useState<unknown>(null);
  const load = useCallback(
    (signal?: AbortSignal) =>
      api
        .importRun(id, signal)
        .then(setRun)
        .catch((e: unknown) => {
          if (!signal?.aborted) setError(e);
        }),
    [id],
  );
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  }, [load]);

  if (error) {
    return (
      <>
        <ErrorState what={t("imports.what.run")} error={describeError(error)} />
        <div className={styles.buttons}>
          <Button onClick={onClose}>{t("imports.back")}</Button>
        </div>
      </>
    );
  }
  if (!run) return <LoadingBlock />;
  return run.state === "preview" ? (
    <Preview run={run} onDone={setRun} onClose={onClose} />
  ) : (
    <Done run={run} onChanged={setRun} onClose={onClose} />
  );
}

function Preview({
  run,
  onDone,
  onClose,
}: {
  run: ImportRun;
  onDone: (run: ImportRun) => void;
  onClose: () => void;
}) {
  const me = useMe();
  const items = useMemo(() => run.items ?? [], [run.items]);
  const [unticked, setUnticked] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const out = useMemo(() => leftOut(items, unticked), [items, unticked]);
  const count = toCreate(items, out);

  function toggle(item: ImportItem) {
    setUnticked((current) => {
      const next = new Set(current);
      if (next.has(item.id)) next.delete(item.id);
      else next.add(item.id);
      return next;
    });
  }

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      onDone(await api.confirmImport(run.id, [...unticked]));
    } catch (e) {
      setError(
        e instanceof ApiError && e.status < 500 ? e.message : describeError(e),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <section className={styles.box} aria-labelledby="import-preview">
        <div className={styles.boxHeader}>
          <h2 id="import-preview" className={styles.title}>
            {t("imports.preview.title", {
              source: run.source_name,
              input: run.input,
            })}
          </h2>
          <StateLabel run={run} />
        </div>
        <p className={styles.note}>
          {multiSite(me) &&
            `${t("imports.preview.where", { site: run.site_name })} `}
          {run.expires_at &&
            t("imports.preview.until", {
              when: formatDateTime(run.expires_at),
            })}
        </p>
        <ul className={styles.counts}>
          {LISTS.map((list) => (
            <li key={list}>
              <Label tone={LIST_TONES[list]} small>
                {t(`imports.list.${list}`)}
              </Label>{" "}
              {run.counts[list]}
            </li>
          ))}
          <li>{t("imports.preview.present", { count: run.counts.present })}</li>
        </ul>
        {error && (
          <p className={styles.error} role="alert">
            {error}
          </p>
        )}
        <div className={styles.buttons}>
          <Button
            variant="k"
            disabled={busy || count === 0}
            onClick={() => void confirm()}
          >
            {busy
              ? t("imports.preview.creating")
              : t("imports.preview.create", { count })}
          </Button>
          <Button onClick={onClose}>{t("imports.preview.cancel")}</Button>
        </div>
        <p className={styles.note}>{t("imports.preview.nothingYet")}</p>
      </section>
      {LISTS.map((list) => (
        <ListSection
          key={list}
          list={list}
          items={items}
          out={out}
          unticked={unticked}
          onToggle={toggle}
        />
      ))}
    </>
  );
}

function ListSection({
  list,
  items,
  out,
  unticked,
  onToggle,
}: {
  list: ImportList;
  items: ImportItem[];
  out?: ReadonlySet<string>;
  unticked?: ReadonlySet<string>;
  onToggle?: (item: ImportItem) => void;
}) {
  const groups = grouped(items, list);
  const titleId = useId();
  if (groups.length === 0) return null;
  return (
    <section className={styles.box} aria-labelledby={titleId}>
      <h2 id={titleId} className={styles.title}>
        <Label tone={LIST_TONES[list]}>{t(`imports.list.${list}`)}</Label>
      </h2>
      <p className={styles.note}>{t(`imports.listNote.${list}`)}</p>
      {list === "not_movable" ? (
        // What stays behind is often long: one line says how much, the list opens on demand.
        <details className={styles.more}>
          <summary>
            {t("imports.showLines", {
              count: groups.reduce((n, g) => n + g.items.length, 0),
            })}
          </summary>
          {groups.map((group) => (
            <Group
              key={group.kind}
              kind={group.kind}
              groupItems={group.items}
              items={items}
              out={out}
              unticked={unticked}
              onToggle={onToggle}
            />
          ))}
        </details>
      ) : (
        groups.map((group) => (
          <Group
            key={group.kind}
            kind={group.kind}
            groupItems={group.items}
            items={items}
            out={out}
            unticked={unticked}
            onToggle={onToggle}
          />
        ))
      )}
    </section>
  );
}

function Group({
  kind,
  groupItems,
  items,
  out,
  unticked,
  onToggle,
}: {
  kind: string;
  groupItems: ImportItem[];
  items: ImportItem[];
  out?: ReadonlySet<string>;
  unticked?: ReadonlySet<string>;
  onToggle?: (item: ImportItem) => void;
}) {
  return (
    <div className={styles.group}>
      <h3 className={styles.groupTitle}>
        {kindHeading(kind)}{" "}
        <span className={styles.note}>{groupItems.length}</span>
      </h3>
      <ul className={styles.items}>
        {groupItems.map((item) => (
          <ItemRow
            key={item.id}
            item={item}
            items={items}
            left={out?.has(item.id) ?? false}
            ticked={!(unticked?.has(item.id) ?? false)}
            onToggle={onToggle}
          />
        ))}
      </ul>
    </div>
  );
}

function ItemRow({
  item,
  items,
  left,
  ticked,
  onToggle,
}: {
  item: ImportItem;
  items: ImportItem[];
  left: boolean;
  ticked: boolean;
  onToggle?: (item: ImportItem) => void;
}) {
  const title = itemTitle(item, items);
  const canTick = onToggle !== undefined && tickable(item);
  // Left out because something it needs is.
  const needsOut = left && ticked;
  return (
    <li className={cx(styles.item, left && styles.itemOut)}>
      {canTick ? (
        <label className={styles.itemHead}>
          <input
            type="checkbox"
            checked={ticked && !needsOut}
            disabled={needsOut}
            onChange={() => onToggle(item)}
          />
          <span className={styles.itemTitle}>{title}</span>
        </label>
      ) : (
        <span className={styles.itemHead}>
          <span className={styles.itemTitle}>{title}</span>
        </span>
      )}
      <span className={styles.itemMeta}>
        {item.present && (
          <Label tone="neutral" small>
            {t("imports.item.present")}
          </Label>
        )}{" "}
        {originKind(item.origin.kind)}: {item.origin.name}
        {item.kind !== "server" &&
          item.hosts.length > 0 &&
          ` · ${item.hosts.length > 3 ? t("imports.item.hosts", { count: item.hosts.length }) : item.hosts.join(", ")}`}
        {needsOut && ` · ${t("imports.item.needsOut")}`}
      </span>
      {item.origin.detail && (
        <code className={styles.detail}>{item.origin.detail}</code>
      )}
      {item.note && (
        <span className={styles.itemNote}>{noteText(item.note)}</span>
      )}
    </li>
  );
}

function Done({
  run,
  onChanged,
  onClose,
}: {
  run: ImportRun;
  onChanged: (run: ImportRun) => void;
  onClose: () => void;
}) {
  const result = run.result;
  const items = run.items ?? [];
  const servers = result?.created.server ?? 0;
  return (
    <>
      <section
        className={styles.box}
        aria-labelledby="import-done"
        aria-live="polite"
      >
        <div className={styles.boxHeader}>
          <h2 id="import-done" className={styles.title}>
            {t("imports.preview.title", {
              source: run.source_name,
              input: run.input,
            })}
          </h2>
          <StateLabel run={run} />
        </div>
        {result && (
          <>
            <p className={styles.note}>
              {createdAnything(result)
                ? t("imports.done.created", {
                    what: countsText(result.created),
                  })
                : t("imports.done.nothing")}{" "}
              {result.present > 0 &&
                t("imports.done.present", { count: result.present })}{" "}
              {result.skipped > 0 &&
                t("imports.done.skipped", { count: result.skipped })}
            </p>
            {result.failed.length > 0 && (
              <Banner tone="warn">
                {t("imports.done.failed", { count: result.failed.length })}
                <ul className={styles.failed}>
                  {result.failed.map((f) => {
                    const item = items.find((i) => i.id === f.item);
                    return (
                      <li key={f.item}>
                        {item ? itemTitle(item, items) : f.item}: {f.error}
                      </li>
                    );
                  })}
                </ul>
              </Banner>
            )}
            {run.state === "confirmed" && servers > 0 && (
              <Banner tone="info">
                {t("imports.done.agents", { count: servers })}{" "}
                <Link href={TOKENS_PATH}>{t("imports.done.addServers")}</Link>
              </Banner>
            )}
            {run.state === "undone" && (
              <p className={styles.note}>
                {t("imports.done.removed", {
                  what: countsText(result.removed),
                })}{" "}
                {result.kept_servers.length > 0 &&
                  t("imports.done.kept", {
                    names: result.kept_servers.join(", "),
                  })}
              </p>
            )}
          </>
        )}
        <div className={styles.buttons}>
          {run.state === "confirmed" && (
            <InlineConfirm
              trigger={t("imports.undo.trigger")}
              title={t("imports.undo.title")}
              consequence={t("imports.undo.consequence")}
              confirmLabel={t("imports.undo.confirm")}
              onConfirm={async () => onChanged(await api.undoImport(run.id))}
              describeError={(e) =>
                e instanceof ApiError && e.status < 500
                  ? e.message
                  : describeError(e)
              }
            />
          )}
          <Button onClick={onClose}>{t("imports.back")}</Button>
        </div>
      </section>
      {LISTS.map((list) => (
        <ListSection key={list} list={list} items={items} />
      ))}
    </>
  );
}
