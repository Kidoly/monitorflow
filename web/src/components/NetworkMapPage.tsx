"use client";

import { useRouter, useSearchParams } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";

import { t } from "@/i18n";
import {
  type MapDocument,
  type MapLabel,
  type MapLive,
  type MapManualLink,
  type MapNode,
  type MapSources,
  type MapZone,
  type NetworkMap,
  api,
  describeError,
} from "@/lib/api";
import { autoLayout, positions } from "@/lib/mapLayout";
import {
  MAP_REFRESH_MS,
  imageBase64,
  linkColor,
  newId,
  nodeColor,
  place,
  placeAll,
  setHidden,
  troubleCounts,
  withDocument,
} from "@/lib/maps";
import { MAPS_PATH, mapHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { usePolledData } from "@/lib/usePolledData";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, fieldClass } from "@/components/ui/Button";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Switch } from "@/components/ui/Switch";

import { MapCanvas, type MapSelection, useBackground } from "./MapCanvas";
import styles from "./Maps.module.css";

const SOURCES: (keyof MapSources)[] = ["relays", "dependencies", "neighbors", "discovered"];

/** The colours of the legend, in the order of how bad they are. */
function Legend() {
  const states = [
    ["critical", t("common.state.critical")],
    ["warning", t("common.state.warning")],
    ["offline", t("maps.legend.down")],
    ["ok", t("common.state.ok")],
    ["unknown", t("maps.state.unknown")],
  ] as const;
  return (
    <ul className={styles.legend} aria-label={t("maps.legend.label")}>
      {states.map(([state, words]) => (
        <li key={state}>
          <i className={styles.swatch} style={{ background: nodeColor(state) }} aria-hidden="true" />
          {words}
        </li>
      ))}
      <li>
        <i className={styles.swatch} style={{ background: linkColor("ok") }} aria-hidden="true" />
        {t("maps.legend.thicker")}
      </li>
    </ul>
  );
}

/** Monitoring › Network maps › one map: live, and editable by operators of its place (ADR-0185). */
export function NetworkMapView() {
  const params = useSearchParams();
  const id = params.get("id") ?? "";
  const router = useRouter();
  const scope = useScope();
  const [map, setMap] = useState<NetworkMap | null>(null);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [editing, setEditing] = useState(params.get("edit") === "1");
  const [draft, setDraft] = useState<MapDocument | null>(null);
  const [name, setName] = useState("");
  const [selected, setSelected] = useState<MapSelection | null>(null);
  const [picking, setPicking] = useState<string[] | null>(null);
  const [fitKey, setFitKey] = useState(0);
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const stage = useRef<HTMLDivElement | null>(null);

  const [reload, setReload] = useState(0);
  const loadMap = () => setReload((n) => n + 1);
  useEffect(() => {
    const controller = new AbortController();
    api.map(id, controller.signal).then(
      (found) => {
        setMap(found);
        setName(found.name);
        setDraft(found.document);
        setLoadError(null);
      },
      (e: unknown) => {
        if (!controller.signal.aborted) setLoadError(e);
      },
    );
    return () => controller.abort();
  }, [id, reload]);

  const fetcher = useCallback((signal: AbortSignal) => api.mapLive(id, signal), [id]);
  const { data: live, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { intervalMs: MAP_REFRESH_MS });
  const background = useBackground(live ?? null);

  if (loadError) {
    const status = (loadError as { status?: number }).status;
    return (
      <Page>
        {status === 404 ? (
          <NotFound title={t("maps.notFound")} />
        ) : (
          <ErrorState what={t("maps.title")} error={describeError(loadError)} onRetry={loadMap} />
        )}
      </Page>
    );
  }
  if (!map || !draft) return <LoadingBlock />;

  const canEdit = map.can_edit;
  const isEditing = editing && canEdit;
  const shown: MapLive | undefined = live && (isEditing ? withDocument(live, draft) : live);
  const at = shown ? positions(shown.nodes, shown.links) : new Map<string, { x: number; y: number }>();
  const nodeByKey = new Map((shown?.nodes ?? []).map((n) => [n.key, n]));

  const change = (next: MapDocument) => {
    setDraft(next);
    setProblem(null);
  };
  const centre = () => {
    const points = [...at.values()];
    if (points.length === 0) return { x: 0, y: 0 };
    return { x: points.reduce((a, p) => a + p.x, 0) / points.length, y: points.reduce((a, p) => a + p.y, 0) / points.length };
  };

  async function save() {
    if (!map || !draft) return;
    if (!name.trim()) return setProblem(t("maps.form.nameRequired"));
    setBusy(true);
    setProblem(null);
    try {
      const saved = await api.updateMap(map.id, {
        name: name.trim(),
        place: { kind: map.place.kind, id: map.place.id },
        document: draft,
        revision: map.revision,
      });
      setMap(saved);
      setDraft(saved.document);
      setEditing(false);
      setSelected(null);
      setPicking(null);
      refresh();
    } catch (e) {
      setProblem(t("maps.edit.saveFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  function cancel() {
    if (!map) return;
    setDraft(map.document);
    setName(map.name);
    setEditing(false);
    setSelected(null);
    setPicking(null);
    setProblem(null);
  }

  function relayOut() {
    if (!shown || !draft) return;
    const visible = shown.nodes.filter((n) => !n.hidden).map((n) => ({ ...n, x: null, y: null }));
    change(placeAll(draft, autoLayout(visible, shown.links)));
    setFitKey((k) => k + 1);
  }

  function addZone() {
    if (!draft) return;
    const c = centre();
    const zone: MapZone = { id: newId(draft, "zone"), label: t("maps.edit.newZone"), x: c.x - 200, y: c.y - 140, width: 400, height: 280, color: 1 };
    change({ ...draft, zones: [...draft.zones, zone] });
    setSelected({ kind: "zone", id: zone.id });
  }

  function addLabel() {
    if (!draft) return;
    const c = centre();
    const label: MapLabel = { id: newId(draft, "label"), text: t("maps.edit.newLabel"), x: c.x, y: c.y, size: "medium" };
    change({ ...draft, labels: [...draft.labels, label] });
    setSelected({ kind: "label", id: label.id });
  }

  function pick(node: MapNode) {
    if (!draft || !picking) return;
    if (picking.length === 0) return setPicking([node.key]);
    if (picking[0] === node.key) return;
    const link: MapManualLink = { id: newId(draft, "link"), from: picking[0]!, to: node.key };
    change({ ...draft, links: [...draft.links, link] });
    setPicking(null);
    setSelected({ kind: "link", id: link.id });
  }

  async function upload(file: File | undefined) {
    if (!file || !map) return;
    const read = await imageBase64(file);
    if ("error" in read) return setProblem(read.error);
    setBusy(true);
    try {
      await api.setMapBackground(map.id, read.data);
      setNotice(t("maps.background.set"));
      refresh();
    } catch (e) {
      setProblem(t("maps.background.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  async function removeBackground() {
    if (!map || !draft) return;
    setBusy(true);
    try {
      await api.removeMapBackground(map.id);
      const { background: _gone, ...rest } = draft;
      void _gone;
      change(rest);
      refresh();
    } catch (e) {
      setProblem(t("maps.background.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  async function readNeighbors() {
    if (!map) return;
    setBusy(true);
    setNotice(t("maps.neighbors.reading"));
    try {
      const read = await api.readMapNeighbors(map.id);
      setNotice(
        read.failed.length
          ? t("maps.neighbors.someFailed", {
              devices: read.devices,
              neighbors: read.neighbors,
              failed: read.failed.map((f) => `${f.name} (${f.error})`).join(", "),
            })
          : t("maps.neighbors.read", { devices: read.devices, neighbors: read.neighbors }),
      );
      refresh();
    } catch (e) {
      setNotice(t("maps.neighbors.failed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  function fullScreen() {
    const element = stage.current as (HTMLDivElement & { webkitRequestFullscreen?: () => void }) | null;
    if (!element) return;
    if (element.requestFullscreen) void element.requestFullscreen().catch(() => undefined);
    else element.webkitRequestFullscreen?.();
  }

  const counts = live ? troubleCounts(live) : null;
  const summary = counts
    ? counts.critical + counts.warning + counts.down === 0
      ? t("maps.summary.allOk")
      : t("maps.summary.trouble", { critical: counts.critical, warning: counts.warning, down: counts.down })
    : null;
  const hidden = (shown?.nodes ?? []).filter((n) => n.hidden);
  const zone = selected?.kind === "zone" ? draft.zones.find((z) => z.id === selected.id) : undefined;
  const label = selected?.kind === "label" ? draft.labels.find((l) => l.id === selected.id) : undefined;
  const link = selected?.kind === "link" ? draft.links.find((l) => l.id === selected.id) : undefined;
  const node = selected?.kind === "node" ? nodeByKey.get(selected.key) : undefined;
  const linkEnds = link ? [nodeByKey.get(link.from), nodeByKey.get(link.to)].filter((n): n is MapNode => !!n?.server_id) : [];

  const editPanel = isEditing && (
    <aside className={styles.side} aria-label={t("maps.edit.label")}>
      <div className={styles.card}>
        <label className={styles.field}>
          {t("maps.form.name")}
          <input className={fieldClass} value={name} maxLength={80} onChange={(e) => setName(e.target.value)} />
        </label>
        <div className={styles.toolbar}>
          <Button size="small" onClick={relayOut}>
            {t("maps.edit.layout")}
          </Button>
          <Button size="small" onClick={addZone}>
            {t("maps.edit.addZone")}
          </Button>
          <Button size="small" onClick={addLabel}>
            {t("maps.edit.addLabel")}
          </Button>
          <Button size="small" active={picking !== null} onClick={() => setPicking(picking ? null : [])}>
            {t("maps.edit.addLink")}
          </Button>
        </div>
        {picking && <p className={styles.hint}>{picking.length === 0 ? t("maps.edit.pickFirst") : t("maps.edit.pickSecond")}</p>}
      </div>

      {node && (
        <div className={styles.card}>
          <h2>{node.name}</h2>
          <Button size="small" onClick={() => change(setHidden(draft, node.key, true, at.get(node.key) ?? { x: 0, y: 0 }))}>
            {t("maps.edit.hide")}
          </Button>
        </div>
      )}
      {zone && (
        <div className={styles.card}>
          <h2>{t("maps.edit.zone")}</h2>
          <label className={styles.field}>
            {t("maps.edit.zoneName")}
            <input
              className={fieldClass}
              value={zone.label}
              maxLength={80}
              onChange={(e) => change({ ...draft, zones: draft.zones.map((z) => (z.id === zone.id ? { ...z, label: e.target.value } : z)) })}
            />
          </label>
          <div className={styles.row}>
            <label className={styles.field}>
              {t("maps.edit.colour")}
              <select
                className={fieldClass}
                value={zone.color}
                onChange={(e) => change({ ...draft, zones: draft.zones.map((z) => (z.id === zone.id ? { ...z, color: Number(e.target.value) } : z)) })}
              >
                {[1, 2, 3, 4, 5, 6, 7, 8].map((c) => (
                  <option key={c} value={c}>
                    {t("maps.edit.colourN", { n: c })}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <Button size="small" variant="danger" onClick={() => {
            change({ ...draft, zones: draft.zones.filter((z) => z.id !== zone.id) });
            setSelected(null);
          }}>
            {t("maps.edit.remove")}
          </Button>
        </div>
      )}
      {label && (
        <div className={styles.card}>
          <h2>{t("maps.edit.labelTitle")}</h2>
          <label className={styles.field}>
            {t("maps.edit.text")}
            <input
              className={fieldClass}
              value={label.text}
              maxLength={200}
              onChange={(e) => change({ ...draft, labels: draft.labels.map((l) => (l.id === label.id ? { ...l, text: e.target.value } : l)) })}
            />
          </label>
          <label className={styles.field}>
            {t("maps.edit.size")}
            <select
              className={fieldClass}
              value={label.size}
              onChange={(e) =>
                change({ ...draft, labels: draft.labels.map((l) => (l.id === label.id ? { ...l, size: e.target.value as MapLabel["size"] } : l)) })
              }
            >
              {(["small", "medium", "large"] as const).map((size) => (
                <option key={size} value={size}>
                  {t(`maps.edit.size.${size}`)}
                </option>
              ))}
            </select>
          </label>
          <Button size="small" variant="danger" onClick={() => {
            change({ ...draft, labels: draft.labels.filter((l) => l.id !== label.id) });
            setSelected(null);
          }}>
            {t("maps.edit.remove")}
          </Button>
        </div>
      )}
      {link && (
        <div className={styles.card}>
          <h2>{t("maps.edit.link")}</h2>
          <label className={styles.field}>
            {t("maps.edit.linkLabel")}
            <input
              className={fieldClass}
              value={link.label ?? ""}
              maxLength={80}
              onChange={(e) =>
                change({
                  ...draft,
                  links: draft.links.map((l) => (l.id === link.id ? { ...l, label: e.target.value || undefined } : l)),
                })
              }
            />
          </label>
          {linkEnds.length > 0 && (
            <div className={styles.row}>
              <label className={styles.field}>
                {t("maps.edit.trafficOf")}
                <select
                  className={fieldClass}
                  value={link.interface?.server_id ?? ""}
                  onChange={(e) =>
                    change({
                      ...draft,
                      links: draft.links.map((l) =>
                        l.id === link.id
                          ? e.target.value
                            ? { ...l, interface: { server_id: e.target.value, instance: l.interface?.instance ?? "" } }
                            : { id: l.id, from: l.from, to: l.to, ...(l.label && { label: l.label }) }
                          : l,
                      ),
                    })
                  }
                >
                  <option value="">{t("maps.edit.noTraffic")}</option>
                  {linkEnds.map((end) => (
                    <option key={end.key} value={end.server_id!}>
                      {end.name}
                    </option>
                  ))}
                </select>
              </label>
              {link.interface && (
                <label className={styles.field}>
                  {t("maps.edit.interface")}
                  <input
                    className={fieldClass}
                    value={link.interface.instance}
                    placeholder="Gi1/0/1"
                    maxLength={256}
                    onChange={(e) =>
                      change({
                        ...draft,
                        links: draft.links.map((l) =>
                          l.id === link.id && l.interface ? { ...l, interface: { ...l.interface, instance: e.target.value } } : l,
                        ),
                      })
                    }
                  />
                </label>
              )}
            </div>
          )}
          <Button size="small" variant="danger" onClick={() => {
            change({ ...draft, links: draft.links.filter((l) => l.id !== link.id) });
            setSelected(null);
          }}>
            {t("maps.edit.remove")}
          </Button>
        </div>
      )}

      <div className={styles.card}>
        <h2>{t("maps.sources.title")}</h2>
        {SOURCES.map((source) => (
          <Switch
            key={source}
            checked={draft.sources[source]}
            onChange={(on) => change({ ...draft, sources: { ...draft.sources, [source]: on } })}
            label={t(`maps.sources.${source}`)}
          />
        ))}
        {(live?.sites.length ?? 0) > 1 && (
          <Switch checked={draft.group_by_site} onChange={(on) => change({ ...draft, group_by_site: on })} label={t("maps.sources.groupBySite")} />
        )}
        <Button size="small" onClick={() => void readNeighbors()} disabled={busy}>
          {t("maps.neighbors.button")}
        </Button>
        {live?.neighbors_read_at && (
          <p className={styles.hint}>
            {t("maps.neighbors.lastRead")} <RelativeTime value={live.neighbors_read_at} now={new Date()} />
          </p>
        )}
      </div>

      <div className={styles.card}>
        <h2>{t("maps.background.title")}</h2>
        <input
          type="file"
          accept="image/png,image/jpeg,image/webp"
          aria-label={t("maps.background.choose")}
          disabled={busy}
          onChange={(e) => void upload(e.target.files?.[0])}
        />
        <p className={styles.hint}>{t("maps.background.hint")}</p>
        {live?.background && (
          <div className={styles.row}>
            <label className={styles.field}>
              {t("maps.background.opacity")}
              <select
                className={fieldClass}
                value={draft.background?.opacity ?? 1}
                onChange={(e) => {
                  const base = draft.background ?? {
                    x: 0,
                    y: 0,
                    width: background?.size?.width ?? 1600,
                    height: background?.size?.height ?? 900,
                  };
                  change({ ...draft, background: { ...base, opacity: Number(e.target.value) } });
                }}
              >
                {[1, 0.75, 0.5, 0.25].map((o) => (
                  <option key={o} value={o}>
                    {`${o * 100}\u00a0%`}
                  </option>
                ))}
              </select>
            </label>
            <Button size="small" variant="danger" onClick={() => void removeBackground()} disabled={busy}>
              {t("maps.background.remove")}
            </Button>
          </div>
        )}
      </div>

      {hidden.length > 0 && (
        <div className={styles.card}>
          <h2>{t("maps.edit.hidden", { count: hidden.length })}</h2>
          <ul className={styles.hiddenList}>
            {hidden.map((n) => (
              <li key={n.key}>
                {n.name}
                <Button size="small" variant="ghost" onClick={() => change(setHidden(draft, n.key, false, { x: n.x ?? 0, y: n.y ?? 0 }))}>
                  {t("maps.edit.show")}
                </Button>
              </li>
            ))}
          </ul>
        </div>
      )}
    </aside>
  );

  const actions = isEditing ? (
    <>
      <Button variant="k" onClick={() => void save()} disabled={busy}>
        {busy ? t("maps.edit.saving") : t("maps.edit.save")}
      </Button>
      <Button onClick={cancel} disabled={busy}>
        {t("maps.edit.cancel")}
      </Button>
    </>
  ) : (
    <>
      <Button onClick={fullScreen}>{t("maps.fullScreen")}</Button>
      {canEdit && (
        <Button variant="k" onClick={() => setEditing(true)}>
          {t("maps.edit.start")}
        </Button>
      )}
    </>
  );

  return (
    <Page>
      <Breadcrumb parents={[{ label: t("maps.title"), href: scopedHref(MAPS_PATH, scope) }]} current={map.name} />
      <PageHeader
        title={map.name}
        meta={
          <>
            {map.place.name}
            {summary && ` · ${summary}`} · <RefreshLine updatedAt={updatedAt} stale={stale} />
          </>
        }
        actions={actions}
      />
      <div className={styles.page}>
        {stale && updatedAt && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
        {problem && (
          <p className={styles.error} role="alert">
            {problem}
          </p>
        )}
        {notice && (
          <p className={styles.hint} role="status">
            {notice}
          </p>
        )}
        {live?.truncated && <p className={styles.hint}>{t("maps.truncated")}</p>}
        <Legend />
        <div className={styles.stage} ref={stage}>
          <div className={styles.main}>
            {!shown ? (
              error ? (
                <ErrorState what={t("maps.title")} error={describeError(error)} onRetry={refresh} />
              ) : (
                <LoadingBlock />
              )
            ) : (
              <MapCanvas
                live={shown}
                mode="page"
                background={background?.src}
                backgroundSize={background?.size}
                editing={isEditing}
                picking={isEditing && picking !== null}
                selected={selected}
                onSelect={setSelected}
                onPick={pick}
                onMoveNode={(key, p) => change(place(draft, key, p.x, p.y))}
                onMoveZone={(zoneId, box) =>
                  change({ ...draft, zones: draft.zones.map((z) => (z.id === zoneId ? { ...z, ...box } : z)) })
                }
                onMoveLabel={(labelId, p) =>
                  change({ ...draft, labels: draft.labels.map((l) => (l.id === labelId ? { ...l, x: p.x, y: p.y } : l)) })
                }
                fitKey={fitKey}
                label={t("maps.canvas.label", { name: map.name })}
              />
            )}
          </div>
          {editPanel}
        </div>
        {isEditing && (
          <InlineConfirm
            trigger={t("maps.delete.trigger")}
            title={t("maps.delete.title", { name: map.name })}
            consequence={t("maps.delete.consequence")}
            confirmLabel={t("maps.delete.confirm")}
            onConfirm={async () => {
              await api.deleteMap(map.id);
              router.push(scopedHref(MAPS_PATH, scope));
            }}
          />
        )}
        {!isEditing && canEdit && live && live.nodes.length === 0 && (
          <p className={styles.hint}>
            {t("maps.emptyMap")}{" "}
            <a href={scopedHref(`${mapHref(map.id)}&edit=1`, scope)}>{t("maps.edit.start")}</a>
          </p>
        )}
      </div>
    </Page>
  );
}
