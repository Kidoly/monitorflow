"use client";

import Link from "next/link";
import { useCallback, useEffect, useId, useState } from "react";

import { t } from "@/i18n";
import {
  type ServerDetail,
  type SnmpCheck,
  type SnmpLookup,
  type SnmpObject,
  type SnmpRead,
  type SnmpTested,
  api,
  describeError,
} from "@/lib/api";
import { formatSeconds } from "@/lib/format";
import { every } from "@/lib/intervals";
import { modifiesSite, useMe } from "@/lib/me";
import { SNMP_LIBRARY_PATH } from "@/lib/routes";
import {
  type ChannelForm,
  type CheckForm,
  MAX_CHANNELS,
  NEW_CHANNEL,
  NEW_CHECK,
  channelOf,
  channelOfRead,
  formOf,
  formProblem,
  formatChannel,
  requestOf,
  stateOf,
} from "@/lib/snmp";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Missing } from "@/components/ui/Missing";
import { Banner } from "@/components/ui/PageState";
import { Label } from "@/components/ui/StatusLabel";
import { TableBox } from "@/components/ui/TableBox";

import admin from "./AdminPages.module.css";
import { ProbeSelect } from "./AddDevice";
import styles from "./ServerPage.module.css";

const INTERVALS = [30_000, 60_000, 300_000, 900_000];
const DURATIONS = [0, 60, 300, 900, 3600];

/** One line of a check's table: a channel, on one row of a table. */
interface ChannelLine {
  key: string;
  name: string;
  oid: string;
  instance: string;
  unit: string;
  value: number | null;
  state: number | null;
  text: string | null;
  limits: string;
}

function limitsText(c: SnmpCheck["channels"][number]): string {
  if (c.limits) {
    const w = c.limits.warning ?? "";
    const k = c.limits.critical ?? "";
    return c.limits.comparison.startsWith("<")
      ? t("snmp.limits.below", { warning: String(w), critical: String(k) })
      : t("snmp.limits.above", { warning: String(w), critical: String(k) });
  }
  return c.state_metric ? t("snmp.limits.onState") : "";
}

function linesOf(check: SnmpCheck): ChannelLine[] {
  return check.channels.flatMap((c) => {
    const values = c.values.length > 0 ? c.values : [{ instance: "", value: null, state: null, text: null, at: null }];
    return values.map((v) => ({
      key: `${c.key}/${v.instance}`,
      name: c.name,
      oid: c.oid,
      instance: v.instance,
      unit: c.unit ?? "",
      value: v.value,
      state: v.state,
      text: v.text,
      limits: limitsText(c),
    }));
  });
}

function Check({
  check,
  server,
  canEdit,
  onEdit,
  onChanged,
}: {
  check: SnmpCheck;
  server: ServerDetail;
  canEdit: boolean;
  onEdit: () => void;
  onChanged: () => void;
}) {
  const probe = check.probe === "relay" ? t("snmp.probe.relay") : t("snmp.probe.central");
  const columns: Column<ChannelLine>[] = [
    {
      key: "channel",
      header: t("snmp.column.channel"),
      cell: (line) => <PrimaryCell title={line.instance ? `${line.name} · ${line.instance}` : line.name} sub={line.oid} />,
    },
    {
      key: "value",
      header: t("snmp.column.value"),
      cell: (line) =>
        line.value !== null ? formatChannel(line.value, line.unit) : line.text !== null ? line.text : <Missing word="noData" />,
    },
    {
      key: "state",
      header: t("snmp.column.state"),
      cell: (line) => {
        const state = stateOf(line.state);
        if (!state) return "";
        return (
          <>
            <Label tone={state.tone}>{state.word}</Label> {line.value !== null && line.text ? line.text : ""}
          </>
        );
      },
    },
    { key: "limits", header: t("snmp.column.limits"), cell: (line) => line.limits },
  ];
  return (
    <TableBox
      title={check.name}
      meta={
        <>
          {t("snmp.runs", { probe, every: every(check.interval_ms) })}
          {check.table && ` · ${t("snmp.table")}`}
          {canEdit && (
            <>
              {" "}
              <Button variant="ghost" size="small" onClick={onEdit}>
                {t("snmp.edit")}
              </Button>
              <InlineConfirm
                trigger={t("snmp.remove")}
                triggerName={`${t("snmp.remove")} ${check.name}`}
                triggerVariant="ghost"
                layout="row"
                title={t("snmp.removeConfirm", { name: check.name })}
                consequence=""
                confirmLabel={`${t("snmp.remove")} ${check.name}`}
                describeError={(e) => t("snmp.removeFailed", { error: describeError(e) })}
                onConfirm={async () => {
                  await api.deleteSnmpCheck(server.id, check.name);
                  onChanged();
                }}
              />
            </>
          )}
        </>
      }
    >
      <DataTable bare dense label={check.name} columns={columns} rows={linesOf(check)} rowKey={(line) => line.key} />
    </TableBox>
  );
}

/**
 * A device's custom SNMP checks (ADR-0157): each check's channels with
 * their last values, states and limits, and the form to add or change one.
 */
export function SnmpChecks({ server, onChanged }: { server: ServerDetail; onChanged?: () => void }) {
  const me = useMe();
  const canEdit = !server.revoked_at && modifiesSite(me, server.customer_id, server.site_id);
  const fetcher = useCallback((signal: AbortSignal) => api.snmpChecks(server.id, signal), [server.id]);
  const { data, error, refresh } = usePolledData(fetcher);
  // null: none open; "": a new check; else the check's name.
  const [editing, setEditing] = useState<string | null>(null);
  const done = () => {
    setEditing(null);
    refresh();
    onChanged?.();
  };
  if (error && !data) return <Banner tone="warn">{t("snmp.loadFailed", { error: describeError(error) })}</Banner>;
  const checks = data?.checks ?? [];
  const current = editing ? checks.find((c) => c.name === editing) : undefined;
  return (
    <section className={styles.websites} aria-label={t("snmp.title")}>
      <h3 className={styles.offTitle}>{t("snmp.title")}</h3>
      {checks.length === 0 && <p className={admin.sectionNote}>{t("snmp.note")}</p>}
      {checks.map((check) =>
        editing === check.name ? null : (
          <Check
            key={check.entry_id}
            check={check}
            server={server}
            canEdit={canEdit}
            onEdit={() => setEditing(check.name)}
            onChanged={done}
          />
        ),
      )}
      {editing !== null && (
        <SnmpCheckForm server={server} initial={current ? formOf(current) : NEW_CHECK} existing={current?.name} onDone={done} onCancel={() => setEditing(null)} />
      )}
      {canEdit && editing === null && (
        <div className={styles.addLine}>
          <Button onClick={() => setEditing("")}>{t("snmp.add")}</Button>
        </div>
      )}
    </section>
  );
}

function ChannelFields({
  n,
  value,
  lookups,
  onChange,
  onRemove,
}: {
  n: number;
  value: ChannelForm;
  lookups: SnmpLookup[];
  onChange: (value: ChannelForm) => void;
  onRemove?: () => void;
}) {
  const set = (patch: Partial<ChannelForm>) => onChange({ ...value, ...patch });
  const text = value.type === "text";
  return (
    <fieldset className={admin.panel}>
      <legend className={admin.sectionTitle}>{t("snmp.channel.title", { n })}</legend>
      <label className={admin.field}>
        {t("snmp.channel.oid")}
        <input className={fieldClass} value={value.oid} spellCheck={false} onChange={(e) => set({ oid: e.target.value })} />
        <span className={admin.sectionNote}>{t("snmp.channel.oidHint")}</span>
      </label>
      <label className={admin.field}>
        {t("snmp.channel.name")}
        <input className={fieldClass} value={value.name} onChange={(e) => set({ name: e.target.value, key: undefined })} />
      </label>
      <label className={admin.field}>
        {t("snmp.channel.type")}
        <select className={fieldClass} value={value.type} onChange={(e) => set({ type: e.target.value as ChannelForm["type"] })}>
          <option value="gauge">{t("snmp.type.gauge")}</option>
          <option value="counter">{t("snmp.type.counter")}</option>
          <option value="text">{t("snmp.type.text")}</option>
        </select>
      </label>
      {!text && (
        <>
          <label className={admin.field}>
            {t("snmp.channel.unit")}
            <input className={fieldClass} value={value.unit} maxLength={16} onChange={(e) => set({ unit: e.target.value })} />
          </label>
          <label className={admin.field}>
            {t("snmp.channel.scale")}
            <select className={fieldClass} value={value.scale} onChange={(e) => set({ scale: e.target.value as ChannelForm["scale"] })}>
              <option value="none">{t("snmp.scale.none")}</option>
              <option value="divide">{t("snmp.scale.divide")}</option>
              <option value="multiply">{t("snmp.scale.multiply")}</option>
            </select>
          </label>
          {value.scale !== "none" && (
            <label className={admin.field}>
              {value.scale === "divide" ? t("snmp.scale.divide") : t("snmp.scale.multiply")}
              <input className={fieldClass} inputMode="decimal" value={value.factor} onChange={(e) => set({ factor: e.target.value })} />
            </label>
          )}
        </>
      )}
      {value.type !== "counter" && (
        <label className={admin.field}>
          {t("snmp.channel.lookup")}
          <select className={fieldClass} value={value.lookup} onChange={(e) => set({ lookup: e.target.value })}>
            <option value="">{t("snmp.lookup.none")}</option>
            {lookups.map((l) => (
              <option key={l.id} value={l.id}>
                {l.builtin ? t("snmp.lookup.builtin", { name: l.name }) : l.name}
              </option>
            ))}
          </select>
        </label>
      )}
      {text ? (
        <>
          <label className={admin.field}>
            {t("snmp.channel.match")}
            <select className={fieldClass} value={value.match} onChange={(e) => set({ match: e.target.value })}>
              <option value="">{t("snmp.match.any")}</option>
              {["contains", "not_contains", "equals", "not_equals", "matches", "not_matches"].map((m) => (
                <option key={m} value={m}>
                  {t(`snmp.match.${m}` as "snmp.match.contains")}
                </option>
              ))}
            </select>
          </label>
          {value.match && (
            <>
              <label className={admin.field}>
                {t("snmp.channel.expect")}
                <input className={fieldClass} value={value.expect} spellCheck={false} onChange={(e) => set({ expect: e.target.value })} />
              </label>
              <label className={admin.field}>
                {t("snmp.channel.severity")}
                <select className={fieldClass} value={value.severity} onChange={(e) => set({ severity: e.target.value as ChannelForm["severity"] })}>
                  <option value="warning">{t("snmp.state.warning")}</option>
                  <option value="critical">{t("snmp.state.critical")}</option>
                </select>
              </label>
            </>
          )}
        </>
      ) : (
        <>
          <label className={admin.field}>
            {t("snmp.channel.limits")}
            <select className={fieldClass} value={value.comparison} onChange={(e) => set({ comparison: e.target.value as ChannelForm["comparison"] })}>
              <option value="">{t("snmp.limits.none")}</option>
              <option value=">">{t("snmp.limits.aboveShort")}</option>
              <option value="<">{t("snmp.limits.belowShort")}</option>
            </select>
          </label>
          {value.comparison && (
            <>
              <label className={admin.field}>
                {t("snmp.channel.warning")}
                <input className={fieldClass} inputMode="decimal" value={value.warning} onChange={(e) => set({ warning: e.target.value })} />
              </label>
              <label className={admin.field}>
                {t("snmp.channel.critical")}
                <input className={fieldClass} inputMode="decimal" value={value.critical} onChange={(e) => set({ critical: e.target.value })} />
              </label>
              <label className={admin.field}>
                {t("snmp.channel.for")}
                <select className={fieldClass} value={String(value.durationS)} onChange={(e) => set({ durationS: Number(e.target.value) })}>
                  {DURATIONS.map((d) => (
                    <option key={d} value={String(d)}>
                      {d === 0 ? t("snmp.for.now") : formatSeconds(d)}
                    </option>
                  ))}
                </select>
              </label>
            </>
          )}
        </>
      )}
      {onRemove && (
        <div className={admin.formFoot}>
          <Button variant="ghost" size="small" onClick={onRemove}>
            {t("snmp.channel.remove")}
          </Button>
        </div>
      )}
    </fieldset>
  );
}

function TestedTable({ tested, types }: { tested: SnmpTested; types: ChannelForm[] }) {
  type Line = { key: string; name: string; read: string; writes: string; note: string };
  const lines: Line[] = tested.rows.flatMap((row) =>
    row.channels.map((c, i) => {
      const channel = types[i];
      const missing = !c.type || c.type.startsWith("noSuch") || c.type === "endOfMibView";
      const state = stateOf(c.state);
      const writes = [c.value !== null ? formatChannel(c.value, channel?.unit) : "", state ? state.word : "", c.text ?? ""]
        .filter(Boolean)
        .join(" · ");
      const note = missing
        ? t("snmp.tested.missing")
        : channel?.type === "counter"
          ? t("snmp.tested.rateNext")
          : c.value === null && c.state === null
            ? t("snmp.tested.notNumber")
            : "";
      return {
        key: `${row.instance}/${c.key}`,
        name: row.instance ? `${c.name} · ${row.instance}` : c.name,
        read: missing ? "" : `${c.type} ${c.raw ?? ""}`,
        writes,
        note,
      };
    }),
  );
  const columns: Column<Line>[] = [
    { key: "channel", header: t("snmp.column.channel"), cell: (l) => l.name },
    { key: "read", header: t("snmp.tested.read"), cell: (l) => l.read || <Missing word="none" /> },
    { key: "writes", header: t("snmp.tested.writes"), cell: (l) => l.writes || l.note },
  ];
  const probe = tested.probe === "central" ? t("snmp.probe.central") : tested.probe;
  return (
    <TableBox title={t("snmp.tested.title", { probe })} meta={tested.cut ? t("snmp.tested.rows", { count: tested.rows.length }) : undefined}>
      <DataTable bare dense label={t("snmp.tested.title", { probe })} columns={columns} rows={lines} rowKey={(l) => l.key} />
    </TableBox>
  );
}

/** Browse: what the device answers under an OID, each to watch with one click. */
export function SnmpBrowse({ server, onPick, onClose }: { server: ServerDetail; onPick: (channel: ChannelForm) => void; onClose: () => void }) {
  const [root, setRoot] = useState("1.3.6.1.4.1");
  const [read, setRead] = useState<SnmpRead | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  async function list() {
    setBusy(true);
    setProblem(null);
    try {
      setRead(await api.readSnmp(server.id, { walk: root.trim(), max: 500 }));
    } catch (e) {
      setProblem(t("snmp.browse.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }
  type Row = SnmpRead["objects"][number];
  const columns: Column<Row>[] = [
    { key: "object", header: t("snmp.browse.object"), cell: (o) => <PrimaryCell title={o.name ?? o.oid} sub={o.name ? o.oid : undefined} /> },
    { key: "type", header: t("snmp.browse.type"), cell: (o) => o.type },
    { key: "value", header: t("snmp.browse.value"), cell: (o) => o.value || <Missing word="none" /> },
  ];
  return (
    // Inside the check's form: a group, not a form of its own.
    <div className={admin.panel} role="group" aria-label={t("snmp.browse.title", { name: server.hostname })}>
      <h2 className={admin.sectionTitle}>{t("snmp.browse.title", { name: server.hostname })}</h2>
      <label className={admin.field}>
        {t("snmp.browse.under")}
        <input
          className={fieldClass}
          value={root}
          spellCheck={false}
          onChange={(e) => setRoot(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void list();
            }
          }}
        />
      </label>
      <div className={admin.formFoot}>
        <Button onClick={() => void list()} disabled={busy}>
          {busy ? t("snmp.browse.listing") : t("snmp.browse.list")}
        </Button>
        <Button onClick={onClose}>{t("snmp.browse.close")}</Button>
      </div>
      {problem && (
        <p className={admin.formError} role="alert">
          {problem}
        </p>
      )}
      {read && read.objects.length === 0 && <p className={admin.sectionNote}>{t("snmp.browse.empty")}</p>}
      {read && read.cut && <p className={admin.sectionNote}>{t("snmp.browse.cut", { count: read.objects.length })}</p>}
      {read && read.objects.length > 0 && (
        <DataTable
          dense
          label={t("snmp.browse.title", { name: server.hostname })}
          columns={columns}
          rows={read.objects}
          rowKey={(o) => o.oid}
          rowActions={(o) => (
            <Button size="small" onClick={() => onPick(channelOfRead(o))} aria-label={`${t("snmp.browse.use")} ${o.name ?? o.oid}`}>
              {t("snmp.browse.use")}
            </Button>
          )}
        />
      )}
    </div>
  );
}

/** Find an object of the library or the MIBs by name, words or OID. */
export function SnmpObjectPicker({ onPick, onClose }: { onPick: (object: SnmpObject) => void; onClose: () => void }) {
  const [q, setQ] = useState("");
  const [results, setFound] = useState<SnmpObject[] | null>(null);
  // Two characters at least, like the API's search.
  const found = q.trim().length < 2 ? null : results;
  useEffect(() => {
    if (q.trim().length < 2) return;
    const controller = new AbortController();
    const timer = setTimeout(() => {
      api.snmpObjects(q.trim(), controller.signal).then(
        (r) => setFound(r.objects),
        () => undefined,
      );
    }, 250);
    return () => {
      clearTimeout(timer);
      controller.abort();
    };
  }, [q]);
  const columns: Column<SnmpObject>[] = [
    {
      key: "object",
      header: t("snmp.browse.object"),
      cell: (o) => (
        <PrimaryCell
          title={o.name}
          sub={`${o.source === "library" ? t("snmp.pick.library", { module: o.module }) : t("snmp.pick.mib", { module: o.module })} · ${o.oid}`}
        />
      ),
    },
    { key: "about", header: "", cell: (o) => o.description },
  ];
  return (
    <div className={admin.panel} role="group" aria-label={t("snmp.pick.title")}>
      <h2 className={admin.sectionTitle}>{t("snmp.pick.title")}</h2>
      <label className={admin.field}>
        {t("snmp.pick.search")}
        <input className={fieldClass} value={q} onChange={(e) => setQ(e.target.value)} />
      </label>
      {found && found.length === 0 && <p className={admin.sectionNote}>{t("snmp.pick.none")}</p>}
      {found && found.length > 0 && (
        <DataTable
          dense
          label={t("snmp.pick.title")}
          columns={columns}
          rows={found}
          rowKey={(o) => `${o.source}/${o.oid}/${o.name}`}
          rowActions={(o) => (
            <Button size="small" onClick={() => onPick(o)} aria-label={`${t("snmp.pick.use")} ${o.name}`}>
              {t("snmp.pick.use")}
            </Button>
          )}
        />
      )}
      <div className={admin.formFoot}>
        <Button onClick={onClose}>{t("snmp.browse.close")}</Button>
      </div>
    </div>
  );
}

/** Add or change a custom check: its channels, Test, Browse and the library. */
export function SnmpCheckForm({
  server,
  initial,
  existing,
  onDone,
  onCancel,
}: {
  server: ServerDetail;
  initial: CheckForm;
  /** The check's name when changing one. */
  existing?: string;
  onDone: () => void;
  onCancel: () => void;
}) {
  const titleId = useId();
  const [form, setForm] = useState<CheckForm>(initial);
  const [lookups, setLookups] = useState<SnmpLookup[]>([]);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState<"save" | "test" | null>(null);
  const [tested, setTested] = useState<SnmpTested | null>(null);
  const [aside, setAside] = useState<"browse" | "pick" | null>(null);
  useEffect(() => {
    const controller = new AbortController();
    api.snmpLookups(controller.signal).then(
      (r) => setLookups(r.lookups),
      () => undefined,
    );
    return () => controller.abort();
  }, []);
  const set = (patch: Partial<CheckForm>) => setForm((f) => ({ ...f, ...patch }));
  const setChannel = (i: number, value: ChannelForm) =>
    setForm((f) => ({ ...f, channels: f.channels.map((c, j) => (j === i ? value : c)) }));
  const add = (channel: ChannelForm) =>
    setForm((f) => {
      const empty = f.channels.length === 1 && f.channels[0]?.oid === "";
      const channels = empty ? [channel] : [...f.channels, channel].slice(0, MAX_CHANNELS);
      return { ...f, channels };
    });
  const pick = (object: SnmpObject) => {
    const s = object.suggestion;
    if (s.table && form.mode === "objects" && form.channels.every((c) => c.oid === "")) {
      setForm((f) => ({ ...f, mode: "table", table: s.table ?? "", nameColumn: s.name_column ?? "" }));
    }
    add(channelOf(object));
    setAside(null);
  };

  async function run(kind: "save" | "test") {
    const found = formProblem(form);
    if (found) return setProblem(found);
    setBusy(kind);
    setProblem(null);
    const body = requestOf(form);
    try {
      if (kind === "test") {
        setTested(await api.testSnmpCheck(server.id, body));
      } else {
        if (existing) await api.updateSnmpCheck(server.id, existing, body);
        else await api.createSnmpCheck(server.id, body);
        onDone();
        return;
      }
    } catch (e) {
      setProblem(kind === "test" ? t("snmp.testFailed", { error: describeError(e) }) : t("snmp.failed", { error: describeError(e) }));
    }
    setBusy(null);
  }

  return (
    <form
      className={admin.panel}
      aria-labelledby={titleId}
      noValidate
      onSubmit={(e) => {
        e.preventDefault();
        void run("save");
      }}
    >
      <h2 id={titleId} className={admin.sectionTitle}>
        {existing ? t("snmp.form.titleEdit", { name: existing }) : t("snmp.form.titleNew")}
      </h2>
      <p className={admin.sectionNote}>{t("snmp.note")}</p>
      {!existing && (
        <label className={admin.field}>
          {t("snmp.form.name")}
          <input className={fieldClass} value={form.name} spellCheck={false} onChange={(e) => set({ name: e.target.value })} />
          <span className={admin.sectionNote}>{t("snmp.form.nameHint")}</span>
        </label>
      )}
      <ProbeSelect siteId={server.site_id} value={form.probe} onChange={(probe) => set({ probe })} label={t("devices.add.from")} />
      <label className={admin.field}>
        {t("snmp.form.interval")}
        <select className={fieldClass} value={String(form.intervalMs)} onChange={(e) => set({ intervalMs: Number(e.target.value) })}>
          {INTERVALS.map((ms) => (
            <option key={ms} value={String(ms)}>
              {formatSeconds(ms / 1000)}
            </option>
          ))}
        </select>
      </label>
      <label className={admin.field}>
        {t("snmp.form.mode")}
        <select className={fieldClass} value={form.mode} onChange={(e) => set({ mode: e.target.value as CheckForm["mode"] })}>
          <option value="objects">{t("snmp.mode.objects")}</option>
          <option value="table">{t("snmp.mode.table")}</option>
        </select>
      </label>
      {form.mode === "table" && (
        <>
          <label className={admin.field}>
            {t("snmp.form.table")}
            <input className={fieldClass} value={form.table} spellCheck={false} onChange={(e) => set({ table: e.target.value })} />
          </label>
          <label className={admin.field}>
            {t("snmp.form.nameColumn")}
            <input className={fieldClass} value={form.nameColumn} spellCheck={false} onChange={(e) => set({ nameColumn: e.target.value })} />
            <span className={admin.sectionNote}>{t("snmp.form.nameColumnHint")}</span>
          </label>
        </>
      )}
      {form.channels.map((channel, i) => (
        <ChannelFields
          key={i}
          n={i + 1}
          value={channel}
          lookups={lookups}
          onChange={(value) => setChannel(i, value)}
          onRemove={form.channels.length > 1 ? () => setForm((f) => ({ ...f, channels: f.channels.filter((_, j) => j !== i) })) : undefined}
        />
      ))}
      <div className={admin.formFoot}>
        {form.channels.length < MAX_CHANNELS && <Button onClick={() => add(NEW_CHANNEL)}>{t("snmp.addChannel")}</Button>}
        <Button onClick={() => setAside("browse")}>{t("snmp.browse")}</Button>
        <Button onClick={() => setAside("pick")}>{t("snmp.pick")}</Button>
        <Link href={SNMP_LIBRARY_PATH}>{t("snmp.manageLibrary")}</Link>
      </div>
      {aside === "browse" && (
        <SnmpBrowse
          server={server}
          onPick={(c) => {
            add(c);
            setAside(null);
          }}
          onClose={() => setAside(null)}
        />
      )}
      {aside === "pick" && <SnmpObjectPicker onPick={pick} onClose={() => setAside(null)} />}
      {tested && <TestedTable tested={tested} types={form.channels} />}
      <div className={admin.formFoot}>
        <Button variant="k" type="submit" disabled={busy !== null}>
          {busy === "save" ? t("snmp.saving") : t("snmp.save")}
        </Button>
        <Button onClick={() => void run("test")} disabled={busy !== null}>
          {busy === "test" ? t("snmp.testing") : t("snmp.test")}
        </Button>
        <Button onClick={onCancel} disabled={busy !== null}>
          {t("snmp.cancel")}
        </Button>
      </div>
      {problem && (
        <p className={admin.formError} role="alert">
          {problem}
        </p>
      )}
    </form>
  );
}
