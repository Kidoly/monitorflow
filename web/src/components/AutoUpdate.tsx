"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import { useCallback, useId, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type AutoUpdateChange,
  type AutoUpdateKind,
  type AutoUpdatePeer,
  type AutoUpdatePeerState,
  type AutoUpdateSite,
  type AutoUpdateStatus,
  type AutoUpdateVersionCount,
  type AutoUpdateVersionPeer,
  type SitePhase,
  api,
  describeError,
} from "@/lib/api";
import { administersEverything, useMe, useMultiCustomer } from "@/lib/me";
import { SITES_PATH, relayHref, serverHref } from "@/lib/routes";
import { type ParamChanges, useUrlParams } from "@/lib/urlState";
import { usePolledData } from "@/lib/usePolledData";

import styles from "./AdminPages.module.css";
import { Button, ButtonLink, fieldClass } from "./ui/Button";
import { type Column, DataTable } from "./ui/DataTable";
import { Banner, ErrorState, LoadingBlock, NoPermission, PageHeader, RefreshLine, StaleBanner } from "./ui/PageState";
import { RelativeTime } from "./ui/RelativeTime";
import { StatTiles } from "./ui/StatTiles";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

/** `/admin/updates/`: global administrators only, like the API (ADR-0055). */
export function AutoUpdatePage() {
  const me = useMe();
  if (!administersEverything(me)) {
    return (
      <NoPermission title={t("autoUpdate.role.title")}>{t("autoUpdate.role.text")}</NoPermission>
    );
  }
  return <AutoUpdate />;
}

const PHASE_TONES: Record<SitePhase, Tone> = {
  canary: "info",
  rolling: "info",
  blocked: "bad",
};

/** One sentence for where the rollout of `kind` stands. */
export function summary(status: AutoUpdateStatus, kind: AutoUpdateKind): [string, Tone] {
  const { settings } = status;
  if (settings.paused) return [t("autoUpdate.summary.paused"), "warn"];
  if (kind.rollout?.state === "halted") {
    // The server's reason stays in its words (ADR-0136).
    return [t("autoUpdate.summary.halted", { reason: kind.rollout.reason ?? t("autoUpdate.summary.tooManyFailed") }), "bad"];
  }
  if (!kind.target) {
    return kind.latest
      ? [t("autoUpdate.summary.serverOlder", { server: status.server_version, latest: kind.latest }), "warn"]
      : [t("autoUpdate.summary.noRelease"), "neutral"];
  }
  const busy = kind.totals.waiting + kind.totals.pending;
  if (busy === 0) return [t("autoUpdate.summary.upToDate", { target: kind.target }), "ok"];
  return [
    t("autoUpdate.summary.rolling", { target: kind.target, pending: kind.totals.pending, waiting: kind.totals.waiting }),
    "info",
  ];
}

function AutoUpdate() {
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(api.autoUpdate);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  async function act(action: () => Promise<unknown>, what: string) {
    setBusy(true);
    setActionError(null);
    try {
      await action();
      refresh();
    } catch (e) {
      setActionError(
        e instanceof ApiError && e.status === 409
          ? t("autoUpdate.refused", { what, reason: e.message })
          : t("autoUpdate.failed", { what, error: describeError(e) }),
      );
    } finally {
      setBusy(false);
    }
  }
  const change = (body: AutoUpdateChange, what: string) => act(() => api.setAutoUpdate(body), what);

  const header = (
    <PageHeader
      title={t("autoUpdate.title")}
      meta={updatedAt !== null || error !== null ? <RefreshLine updatedAt={updatedAt} stale={stale} /> : undefined}
    />
  );
  if (error instanceof ApiError && error.status === 403) {
    return (
      <div className={styles.page}>
        {header}
        <NoPermission title={t("autoUpdate.role.title")} />
      </div>
    );
  }
  if (!data || !updatedAt) {
    return (
      <div className={styles.page}>
        {header}
        {error ? <ErrorState what={t("autoUpdate.what")} error={describeError(error)} onRetry={refresh} /> : <LoadingBlock />}
      </div>
    );
  }

  const agents = data.agents;
  const [text, tone] = summary(data, agents);
  const halted = [data.relays.rollout, agents.rollout].find((r) => r?.state === "halted") ?? null;
  // Progress is told by the tiles below; the banner is for what holds updates back.
  const quiet = tone === "ok" || tone === "info";
  return (
    <div className={styles.page}>
      {header}
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {actionError && <Banner tone="bad">{actionError}</Banner>}
      <p className={styles.sectionNote}>{versionsText(data)}</p>
      {!quiet && (
        <Banner tone={tone === "neutral" ? "plain" : tone} role={tone === "bad" ? "alert" : "status"}>
          {text}
          {halted && (
            <>
              {" "}
              <Button
                size="small"
                disabled={busy}
                onClick={() =>
                  void act(
                    () => api.resumeRollout(halted.kind, halted.version),
                    t("autoUpdate.resuming", { version: halted.version }),
                  )
                }
              >
                {t("autoUpdate.resume", { version: halted.version })}
              </Button>
            </>
          )}
        </Banner>
      )}
      <Settings status={data} busy={busy} onChange={change} />
      <Kind relays kind={data.relays} />
      <Kind relays={false} kind={agents} />
    </div>
  );
}

/** The release peers move to, the newest one held and the server's, once for both kinds. */
export function versionsText(status: AutoUpdateStatus): string {
  const { target, latest } = status.agents;
  if (target && target === latest && target === status.server_version) {
    return t("autoUpdate.factsSame", { version: target });
  }
  return t("autoUpdate.facts", {
    target: target ?? t("autoUpdate.none"),
    latest: latest ?? t("autoUpdate.none"),
    server: status.server_version,
  });
}

const PEER_STATES: readonly AutoUpdatePeerState[] = ["up_to_date", "waiting", "pending", "failed"];

/** In the URL, the version of those that reported none: never a real one. */
const NO_VERSION = "none";

/**
 * The list of peers open under the tiles of `relays` or agents, from the
 * URL: `?agents=waiting&agentsSite=<id>`, or the peers on one version:
 * `?agentsVersion=0.9.1`, so it can be shared. One list at a time.
 */
function useOpenList(relays: boolean) {
  const [params, setParams] = useUrlParams();
  const pathname = usePathname();
  const key = relays ? "relays" : "agents";
  const siteKey = `${key}Site`;
  const versionKey = `${key}Version`;
  const value = params.get(key);
  const state = PEER_STATES.find((s) => s === value) ?? null;
  const site = state ? params.get(siteKey) : null;
  const versionParam = state ? null : params.get(versionKey);
  /** `undefined` when no version is open, `null` for "not reported". */
  const version = versionParam === null ? undefined : versionParam === NO_VERSION ? null : versionParam;
  const changes = (next: AutoUpdatePeerState | null, nextSite: string | null): ParamChanges => ({
    [key]: next,
    [siteKey]: next ? nextSite : null,
    [versionKey]: null,
  });
  const href = (next: AutoUpdatePeerState, nextSite: string) => {
    const query = new URLSearchParams(params);
    query.set(key, next);
    query.set(siteKey, nextSite);
    return `${pathname}?${query}`;
  };
  return {
    state,
    site,
    /** Opens `next` for every site, or closes it when it is open. */
    toggle: (next: AutoUpdatePeerState) => setParams(changes(state === next ? null : next, null)),
    open: (next: AutoUpdatePeerState, nextSite: string | null) => setParams(changes(next, nextSite)),
    href,
    version,
    /** Opens the peers on `next`, or closes them when they are open. */
    toggleVersion: (next: string | null) =>
      setParams({
        ...changes(null, null),
        [versionKey]: version === next ? null : (next ?? NO_VERSION),
      }),
    versionHref: (next: string | null) => {
      const query = new URLSearchParams(params);
      query.delete(key);
      query.delete(siteKey);
      query.set(versionKey, next ?? NO_VERSION);
      return `${pathname}?${query}`;
    },
  };
}

/** Progress of one kind of peer: totals, sites and rollouts. */
function Kind({ relays, kind }: { relays: boolean; kind: AutoUpdateKind }) {
  const title = relays ? t("autoUpdate.relays") : t("autoUpdate.agents");
  const list = useOpenList(relays);
  const listId = useId();
  const versionListId = useId();
  const tile = (label: string, state: AutoUpdatePeerState, value: number, tone: Tone) => ({
    label,
    value,
    tone,
    active: list.state === state,
    controls: listId,
    // A zero has nothing to list, unless the URL opened it.
    onToggle: value > 0 || list.state === state ? () => list.toggle(state) : undefined,
  });
  const openSite = (state: AutoUpdatePeerState, site: string) => {
    list.open(state, site);
    requestAnimationFrame(() => document.getElementById(listId)?.scrollIntoView?.({ block: "nearest" }));
  };
  return (
    <section className={styles.section} aria-label={title}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{title}</h2>
      </div>
      {!relays && kind.totals.waiting_for_relay > 0 && (
        <p className={styles.sectionNote}>{t("autoUpdate.waitingForRelay", { count: kind.totals.waiting_for_relay })}</p>
      )}
      <StatTiles
        dense
        label={title}
        tiles={[
          tile(t("autoUpdate.tile.upToDate"), "up_to_date", kind.totals.up_to_date, "ok"),
          tile(t("autoUpdate.tile.waiting"), "waiting", kind.totals.waiting, "neutral"),
          tile(t("autoUpdate.tile.inProgress"), "pending", kind.totals.pending, "info"),
          tile(t("autoUpdate.tile.failed"), "failed", kind.totals.failed, kind.totals.failed > 0 ? "bad" : "neutral"),
        ]}
      />
      {list.state && (
        <Peers
          id={listId}
          relays={relays}
          state={list.state}
          site={kind.sites.find((s) => s.id === list.site) ?? null}
          onAllSites={() => list.open(list.state!, null)}
        />
      )}
      <Sites
        sites={kind.sites}
        label={relays ? t("autoUpdate.relaysBySite") : t("autoUpdate.agentsBySite")}
        relays={relays}
        href={list.href}
        onOpen={openSite}
      />
      <Versions
        versions={kind.versions}
        relays={relays}
        open={list.version}
        controls={versionListId}
        href={list.versionHref}
        onToggle={list.toggleVersion}
      />
      {list.version !== undefined && <VersionPeers id={versionListId} relays={relays} version={list.version} />}
    </section>
  );
}

function Settings({
  status,
  busy,
  onChange,
}: {
  status: AutoUpdateStatus;
  busy: boolean;
  onChange: (change: AutoUpdateChange, what: string) => void;
}) {
  const { settings } = status;
  const thresholdId = useId();
  const [threshold, setThreshold] = useState(String(settings.failure_threshold));
  const wanted = Number(threshold);
  const validThreshold = Number.isInteger(wanted) && wanted >= 1 && wanted <= 1000;
  return (
    <section className={styles.section} aria-label={t("autoUpdate.settings")}>
      <div className={styles.sectionHead}>
        <h2 className={styles.sectionTitle}>{t("autoUpdate.settings")}</h2>
      </div>
      <div className={styles.sectionNote}>
        <p className={styles.learnText}>{t("autoUpdate.explain")}</p>
        <details className={styles.learnMore}>
          <summary>{t("autoUpdate.learnMore")}</summary>
          <p className={styles.learnText}>{t("autoUpdate.explainMore")}</p>
        </details>
      </div>
      <div className={styles.buttons}>
        <span>
          {t("autoUpdate.serverWide")} <strong>{settings.enabled ? t("autoUpdate.on") : t("autoUpdate.off")}</strong>
          {settings.enabled_locked ? (
            <span className={styles.dim}>{t("autoUpdate.locked")}</span>
          ) : (
            <>
              {" "}
              <Button
                size="small"
                disabled={busy}
                onClick={() =>
                  onChange(
                    { enabled: !settings.enabled },
                    settings.enabled ? t("autoUpdate.turningOff") : t("autoUpdate.turningOn"),
                  )
                }
              >
                {settings.enabled ? t("autoUpdate.turnOff") : t("autoUpdate.turnOn")}
              </Button>
            </>
          )}
        </span>
        <Button
          size="small"
          disabled={busy}
          onClick={() =>
            onChange({ paused: !settings.paused }, settings.paused ? t("autoUpdate.resumingAll") : t("autoUpdate.pausing"))
          }
        >
          {settings.paused ? t("autoUpdate.resumeAll") : t("autoUpdate.pauseAll")}
        </Button>
        <ButtonLink size="small" href={SITES_PATH}>
          {t("autoUpdate.overrides")}
        </ButtonLink>
      </div>
      <form
        className={styles.newSite}
        onSubmit={(e) => {
          e.preventDefault();
          if (validThreshold) onChange({ failure_threshold: wanted }, t("autoUpdate.changingThreshold"));
        }}
      >
        <label className={styles.field} htmlFor={thresholdId}>
          {t("autoUpdate.threshold")}
        </label>
        <input
          id={thresholdId}
          className={fieldClass}
          type="number"
          min={1}
          max={1000}
          value={threshold}
          onChange={(e) => setThreshold(e.target.value)}
        />
        <Button type="submit" size="small" disabled={busy || !validThreshold || wanted === settings.failure_threshold}>
          {t("autoUpdate.save")}
        </Button>
      </form>
    </section>
  );
}

const LIST_TITLES = {
  agent: {
    up_to_date: "autoUpdate.list.agent.upToDate",
    waiting: "autoUpdate.list.agent.waiting",
    pending: "autoUpdate.list.agent.pending",
    failed: "autoUpdate.list.agent.failed",
  },
  relay: {
    up_to_date: "autoUpdate.list.relay.upToDate",
    waiting: "autoUpdate.list.relay.waiting",
    pending: "autoUpdate.list.relay.pending",
    failed: "autoUpdate.list.relay.failed",
  },
} as const;

/** The agents or relays behind one tile, for every site or `site`. */
function Peers({
  id,
  relays,
  state,
  site,
  onAllSites,
}: {
  id: string;
  relays: boolean;
  state: AutoUpdatePeerState;
  site: AutoUpdateSite | null;
  onAllSites: () => void;
}) {
  const kind = relays ? "relay" : "agent";
  const siteId = site?.id ?? null;
  const fetcher = useCallback(
    (signal: AbortSignal) => api.autoUpdatePeers(kind, state, siteId, signal),
    [kind, state, siteId],
  );
  const { data, error, updatedAt, refresh } = usePolledData(fetcher);
  const multiCustomer = useMultiCustomer();
  const title = t(LIST_TITLES[kind][state]);
  const label = site ? t("autoUpdate.list.inSite", { list: title, site: site.name }) : title;

  let body;
  if (!data || !updatedAt) {
    body = error ? (
      <Banner tone="bad">
        <strong>{t("ui.page.loadFailed", { what: t("autoUpdate.list.what") })}</strong> {describeError(error)}{" "}
        <Button size="small" onClick={refresh}>
          {t("ui.page.retry")}
        </Button>
      </Banner>
    ) : (
      <LoadingBlock />
    );
  } else {
    const now = updatedAt;
    const columns: Column<AutoUpdatePeer>[] = [
      {
        key: "name",
        header: relays ? t("autoUpdate.column.relay") : t("autoUpdate.column.server"),
        cell: (p) => <Link href={relays ? relayHref(p.id) : serverHref(p.id)}>{p.name}</Link>,
      },
      {
        key: "site",
        header: t("autoUpdate.column.site"),
        cell: (p) => (
          <>
            {p.site_name} {multiCustomer && <span className={styles.dim}>· {p.customer_name}</span>}
          </>
        ),
      },
      {
        key: "version",
        header: t("autoUpdate.column.version"),
        cell: (p) => p.version ?? <span className={styles.dim}>{t("autoUpdate.list.noVersion")}</span>,
      },
      {
        key: "details",
        header: t("autoUpdate.column.details"),
        className: styles.wrap,
        cell: (p) => <PeerDetails peer={p} now={now} />,
      },
    ];
    body = (
      <>
        <DataTable bare label={label} columns={columns} rows={data.peers} rowKey={(p) => p.id} />
        {data.total > data.peers.length && (
          <p className={styles.sectionNote}>
            {t("autoUpdate.list.more", { shown: data.peers.length, total: data.total })}
          </p>
        )}
      </>
    );
  }
  return (
    <div id={id} role="region" aria-label={label}>
      <div className={styles.sectionHead}>
        <h3 className={styles.sectionTitle}>{label}</h3>
        {site && (
          <Button size="small" onClick={onAllSites}>
            {t("autoUpdate.list.allSites")}
          </Button>
        )}
      </div>
      {body}
    </div>
  );
}

function PeerDetails({ peer, now }: { peer: AutoUpdatePeer; now: Date }) {
  if (peer.state === "failed") {
    return (
      <>
        {/* The peer's own words (ADR-0136). */}
        {peer.error ?? t("autoUpdate.list.noError")}
        {peer.finished_at && (
          <span className={styles.dim}>
            {" · "}
            <RelativeTime value={peer.finished_at} now={now} />
          </span>
        )}
      </>
    );
  }
  if (peer.waiting_for_relay) return <>{t("autoUpdate.list.forRelay")}</>;
  return null;
}

function Sites({
  sites,
  label,
  relays,
  href,
  onOpen,
}: {
  sites: AutoUpdateSite[];
  label: string;
  relays: boolean;
  href: (state: AutoUpdatePeerState, site: string) => string;
  onOpen: (state: AutoUpdatePeerState, site: string) => void;
}) {
  const multiCustomer = useMultiCustomer();
  /** "3 waiting", a link opening the list of those 3 when not zero. */
  const count = (site: AutoUpdateSite, state: AutoUpdatePeerState, text: string, value: number) =>
    value === 0 ? (
      text
    ) : (
      <a
        href={href(state, site.id)}
        onClick={(e) => {
          e.preventDefault();
          onOpen(state, site.id);
        }}
      >
        {text}
      </a>
    );
  const columns: Column<AutoUpdateSite>[] = [
    {
      key: "site",
      header: t("autoUpdate.column.site"),
      cell: (s) => (
        <>
          <strong>{s.name}</strong> {multiCustomer && <span className={styles.dim}>· {s.customer_name}</span>}
        </>
      ),
    },
    // Only sites that override the server-wide setting say anything here.
    ...(sites.some((s) => s.auto_update !== null)
      ? [
          {
            key: "policy",
            header: t("autoUpdate.column.automatic"),
            cell: (s: AutoUpdateSite) =>
              s.auto_update === null ? null : s.auto_update ? t("autoUpdate.on") : t("autoUpdate.off"),
          },
        ]
      : []),
    {
      key: "phase",
      header: t("autoUpdate.column.rollout"),
      cell: (s) => {
        if (!s.phase) return null;
        return <Label tone={PHASE_TONES[s.phase]}>{t(`autoUpdate.phase.${s.phase}`)}</Label>;
      },
    },
    {
      key: "progress",
      header: relays ? t("autoUpdate.relays") : t("autoUpdate.agents"),
      cell: (s) => (
        <span className={styles.dim}>
          {count(s, "up_to_date", t("autoUpdate.count.upToDate", { count: s.totals.up_to_date }), s.totals.up_to_date)}
          {" · "}
          {count(s, "pending", t("autoUpdate.count.inProgress", { count: s.totals.pending }), s.totals.pending)}
          {" · "}
          {count(s, "waiting", t("autoUpdate.count.waiting", { count: s.totals.waiting }), s.totals.waiting)}
          {s.totals.failed > 0 && (
            <>
              {" · "}
              {count(s, "failed", t("autoUpdate.count.failed", { count: s.totals.failed }), s.totals.failed)}
            </>
          )}
        </span>
      ),
    },
  ];
  return <DataTable bare label={label} columns={columns} rows={sites} rowKey={(s) => s.id} />;
}

/** "0.9.1", or "Not reported" for those that reported no version. */
function versionText(version: string | null) {
  return version ?? <span className={styles.dim}>{t("autoUpdate.list.noVersion")}</span>;
}

/** The versions agents or relays run, each opening the list of who runs it. */
function Versions({
  versions,
  relays,
  open,
  controls,
  href,
  onToggle,
}: {
  versions: AutoUpdateVersionCount[];
  relays: boolean;
  open: string | null | undefined;
  controls: string;
  href: (version: string | null) => string;
  onToggle: (version: string | null) => void;
}) {
  if (versions.length === 0) return null;
  const label = relays ? t("autoUpdate.versions.relays") : t("autoUpdate.versions.agents");
  const columns: Column<AutoUpdateVersionCount>[] = [
    {
      key: "version",
      header: t("autoUpdate.column.version"),
      cell: (v) => <strong>{versionText(v.version)}</strong>,
    },
    {
      key: "state",
      header: t("autoUpdate.column.state"),
      cell: (v) => (v.up_to_date ? <Label tone="ok">{t("autoUpdate.tile.upToDate")}</Label> : null),
    },
    {
      key: "count",
      header: relays ? t("autoUpdate.relays") : t("autoUpdate.agents"),
      cell: (v) => (
        <a
          href={href(v.version)}
          aria-expanded={open === v.version}
          aria-controls={open === v.version ? controls : undefined}
          onClick={(e) => {
            e.preventDefault();
            onToggle(v.version);
          }}
        >
          {t(relays ? "autoUpdate.versions.relayCount" : "autoUpdate.versions.agentCount", { count: v.count })}
        </a>
      ),
    },
  ];
  return (
    <>
      <div className={styles.sectionHead}>
        <h3 className={styles.sectionTitle}>{label}</h3>
      </div>
      <DataTable bare label={label} columns={columns} rows={versions} rowKey={(v) => v.version ?? NO_VERSION} />
    </>
  );
}

/** The agents or relays on one version, with where each stands. */
function VersionPeers({ id, relays, version }: { id: string; relays: boolean; version: string | null }) {
  const kind = relays ? "relay" : "agent";
  const fetcher = useCallback(
    (signal: AbortSignal) => api.autoUpdateVersionPeers(kind, version, signal),
    [kind, version],
  );
  const { data, error, refresh } = usePolledData(fetcher);
  const multiCustomer = useMultiCustomer();
  const label =
    version === null
      ? t(relays ? "autoUpdate.versions.relaysNone" : "autoUpdate.versions.agentsNone")
      : relays
        ? t("autoUpdate.versions.relaysOn", { version })
        : t("autoUpdate.versions.agentsOn", { version });

  let body;
  if (!data) {
    body = error ? (
      <Banner tone="bad">
        <strong>{t("ui.page.loadFailed", { what: t("autoUpdate.list.what") })}</strong> {describeError(error)}{" "}
        <Button size="small" onClick={refresh}>
          {t("ui.page.retry")}
        </Button>
      </Banner>
    ) : (
      <LoadingBlock />
    );
  } else {
    const columns: Column<AutoUpdateVersionPeer>[] = [
      {
        key: "name",
        header: relays ? t("autoUpdate.column.relay") : t("autoUpdate.column.server"),
        cell: (p) => <Link href={relays ? relayHref(p.id) : serverHref(p.id)}>{p.name}</Link>,
      },
      {
        key: "site",
        header: t("autoUpdate.column.site"),
        cell: (p) => (
          <>
            {p.site_name} {multiCustomer && <span className={styles.dim}>· {p.customer_name}</span>}
          </>
        ),
      },
      {
        key: "state",
        header: t("autoUpdate.column.state"),
        cell: (p) => (p.state ? <Label tone={STATE_TONES[p.state]}>{t(STATE_WORDS[p.state])}</Label> : null),
      },
    ];
    body = (
      <>
        <DataTable bare label={label} columns={columns} rows={data.peers} rowKey={(p) => p.id} />
        {data.total > data.peers.length && (
          <p className={styles.sectionNote}>
            {t("autoUpdate.list.more", { shown: data.peers.length, total: data.total })}
          </p>
        )}
      </>
    );
  }
  return (
    <div id={id} role="region" aria-label={label}>
      <div className={styles.sectionHead}>
        <h3 className={styles.sectionTitle}>{label}</h3>
      </div>
      {body}
    </div>
  );
}

const STATE_TONES: Record<AutoUpdatePeerState, Tone> = {
  up_to_date: "ok",
  waiting: "neutral",
  pending: "info",
  failed: "bad",
};

const STATE_WORDS = {
  up_to_date: "autoUpdate.tile.upToDate",
  waiting: "autoUpdate.tile.waiting",
  pending: "autoUpdate.tile.inProgress",
  failed: "autoUpdate.tile.failed",
} as const;
