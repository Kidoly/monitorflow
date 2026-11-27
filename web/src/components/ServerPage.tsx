"use client";

import { useSearchParams } from "next/navigation";
import Link from "next/link";
import { useCallback, useEffect, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type Command,
  type EffectiveSensor,
  type EffectiveSensors,
  type Incident,
  type ServerDetail,
  api,
  describeError,
  isUnauthorized,
  isAbortError,
} from "@/lib/api";
import { updateAvailable } from "@/lib/agentVersion";
import { certificateProblem, certificateState } from "@/lib/certificates";
import { metricsError } from "@/lib/charts";
import { aiCommands, aiSwitch, commandSummary, unsupportedReason } from "@/lib/commandKinds";
import { isActive } from "@/lib/commands";
import { formatOs } from "@/lib/format";
import { administersCustomer, administersSite, modifiesSite, multiCustomer, multiSite, useMe } from "@/lib/me";
import { hasDesktopTab } from "@/lib/remote";
import { serverHref, siteHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { type SensorValues, sensorRows, valuesBySensor } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { Button, ButtonLink } from "@/components/ui/Button";
import { FactsBox } from "@/components/ui/FactsBox";
import { Hint } from "@/components/ui/Hint";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { WaitingForAgent } from "@/components/imports/WaitingForAgent";
import { Banner, ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { RowMenu } from "@/components/ui/RowMenu";
import { Label, StatusLabel, statusWord } from "@/components/ui/StatusLabel";
import { TabPanel, Tabs } from "@/components/ui/Tabs";
import { FlowsTab } from "@/components/FlowsTab";
import { formatExact } from "@/components/ui/time";

import { AllowCommandsLine } from "./AllowCommands";
import { DependenciesTab, UnreachableBanner } from "./Dependencies";
import { DeviceFacts, EditDevice } from "./DeviceDetails";
import { VirtualizationSection } from "./Virtualization";
import { serverSiblings } from "./breadcrumbSiblings";
import { OfflineAlerts } from "./OfflineAlerts";
import { RemovedPanel, RemovePeer } from "./RemovePeer";
import { InventoryTab } from "./InventoryTab";
import { ServerCommandsTab } from "./ServerCommandsTab";
import { RollbackBadge, RollbackFact, useRollback } from "./ServerRollback";
import { ServerLogsTab } from "./ServerLogsTab";
import { type SensorRules, ServerSensorsTab } from "./ServerSensorsTab";
import { ServerDesktopTab } from "./ServerDesktopTab";
import { ServerTerminalTab } from "./ServerTerminalTab";
import { ServerUpdatesTab } from "./ServerUpdatesTab";
import styles from "./ServerPage.module.css";
import { type SilenceBarParts, useSilenceBar } from "./Silences";

/** Commands loaded at first; "Load more" adds pages of this size. */
export const COMMANDS_PAGE = 50;
/** The API's largest page, which caps what a refresh reloads. */
const MAX_LIMIT = 500;

const TABS = ["sensors", "dependencies", "flows", "logs", "updates", "commands", "terminal", "desktop", "inventory"] as const;
export type ServerTab = (typeof TABS)[number];

/** The server page for the `id` query parameter. */
export function ServerView() {
  const id = useSearchParams().get("id");
  // A new ID starts from a clean state instead of showing the old server.
  return <ServerPage key={id} id={id} />;
}

/** Everything the server page shows, loaded in one cycle every 15 s. */
export interface ServerData {
  server: ServerDetail;
  /** The effective sensor configuration, or why it is missing. */
  sensors: EffectiveSensors | { error: string };
  /** The recent values of each sensor's primary metric. */
  values: Map<EffectiveSensor, SensorValues>;
  /** What the sparklines cover, in Unix seconds; null when unreadable. */
  window: { start: number; end: number } | null;
  /** Null when the incidents could not be read. */
  incidents: Incident[] | null;
  commands: { commands: Command[]; nextCursor: string | null } | { error: string };
  /** The newest agent release the server holds (ADR-0053); null when none or unreadable. */
  latestAgent: string | null;
  /** Each sensor's rule and where it comes from, for the Sensors tab. */
  rules: SensorRules;
}

/** `promise`, or `fallback(error)` when it fails for another reason than an abort. */
function settle<T, F>(promise: Promise<T>, fallback: (error: unknown) => F): Promise<T | F> {
  return promise.catch((e: unknown) => {
    if (isAbortError(e)) throw e;
    return fallback(e);
  });
}

async function loadServer(id: string, commandLimit: number, signal: AbortSignal): Promise<ServerData> {
  const sensors = settle(api.serverSensors(id, signal), (e) => ({ error: describeError(e) }));
  // Every sensor's recent values in one request (sparklines, last value, late data).
  const recent = settle(api.sensorValues(id, signal), (e) => ({ error: metricsError(e) }));
  const values = Promise.all([sensors, recent]).then(([config, list]) => {
    const all = "sensors" in config ? config.sensors : [];
    if ("error" in list) return new Map<EffectiveSensor, SensorValues>(all.map((s) => [s, { error: list.error }]));
    return valuesBySensor(all, list);
  });
  const window = recent.then((list) => ("error" in list ? null : { start: list.start, end: list.end }));
  const orNull = <T,>(promise: Promise<T>) => settle(promise, () => null);
  const siteSensors = sensors.then((config) =>
    "site_id" in config ? orNull(api.siteSensors(config.site_id, signal).then((s) => s.sensors)) : null,
  );
  const [server, config, latest, sparkWindow, incidents, commands, releases, thresholds, ownRows, site] = await Promise.all([
    api.server(id, signal),
    sensors,
    values,
    window,
    settle(
      api.incidents({ state: "open", server_id: id, limit: MAX_LIMIT }, signal).then((p) => p.incidents),
      () => null,
    ),
    settle(
      api
        .serverCommands(id, { limit: commandLimit }, signal)
        .then((p) => ({ commands: p.commands, nextCursor: p.next_cursor })),
      (e) => ({ error: describeError(e) }),
    ),
    settle(api.agentReleases(signal), () => null),
    orNull(api.serverThresholds(id, signal).then((t) => t.thresholds)),
    orNull(api.sensorRows({ kind: "servers", id }, signal).then((r) => r.entries)),
    siteSensors,
  ]);
  return {
    server,
    sensors: config,
    values: latest,
    window: sparkWindow,
    incidents,
    commands,
    latestAgent: releases?.latest ?? null,
    rules: { thresholds, ownRows, siteSensors: site },
  };
}

export function ServerPage({ id }: { id: string | null }) {
  const me = useMe();
  const valid = id !== null && UUID.test(id);
  const [params, setParams] = useUrlParams();
  const tab: ServerTab = TABS.find((t) => t === params.get("tab")) ?? "sensors";
  // Alerts and checks became part of Sensors: old links land there.
  const oldTab = params.get("tab") === "alerts";
  useEffect(() => {
    if (oldTab) setParams({ tab: null });
  }, [oldTab, setParams]);
  // Commands shown, so a refresh reloads all of them rather than the first page.
  const [commandLimit, setCommandLimit] = useState(COMMANDS_PAGE);
  // The Details panel under the header; the Commands tab opens it to turn AI commands on.
  const [details, setDetails] = useState(false);
  const fetcher = useCallback((signal: AbortSignal) => loadServer(id ?? "", commandLimit, signal), [id, commandLimit]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });
  const loaded = data?.server;
  const silences = useSilenceBar(
    loaded
      ? { scope: "server", id: loaded.id, name: loaded.hostname, customerId: loaded.customer_id, siteId: loaded.site_id }
      : null,
  );

  useEffect(() => {
    if (data) document.title = `${data.server.hostname} · MonitorFlow`;
  }, [data]);

  if (!valid || (error instanceof ApiError && error.status === 404)) {
    return (
      <NotFound
        title={t("server.notFound")}
        actions={<ButtonLink href="/">{t("server.back")}</ButtonLink>}
      />
    );
  }
  if (!data || !updatedAt) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what={t("server.what")} error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label={t("server.loading")} />
    );
  }

  const { server, sensors, values, incidents, commands, latestAgent } = data;
  const device = server.kind === "device";
  const rows = "sensors" in sensors ? sensorRows(sensors.sensors, values, incidents ?? []) : [];
  const checkNames = "sensors" in sensors ? [...new Set(sensors.sensors.map((s) => s.name))].sort() : [];
  // A device has no agent: only its checks, dependencies and traffic flows
  // (ADR-0148, ADR-0184); a server has no flows tab. Remote terminals
  // (ADR-0190): for those who may open them or see their sessions.
  const terminal = !device && !!server.remote_terminal && (server.remote_terminal.can_open || server.remote_terminal.can_review);
  // Remote desktops: Windows servers only, the same people.
  const desktop = hasDesktopTab(server);
  const shown: ServerTab = device
    ? tab === "dependencies" || tab === "flows"
      ? tab
      : "sensors"
    : tab === "flows" || (tab === "terminal" && !terminal) || (tab === "desktop" && !desktop)
      ? "sensors"
      : tab;
  const active = "commands" in commands ? commands.commands.filter(isActive).length : 0;

  return (
    <Page>
      <ServerHeading server={server} />
      <ServerHeader
        server={server}
        latestAgent={latestAgent}
        commands={commands}
        now={updatedAt}
        stale={stale}
        onChanged={refresh}
        details={details}
        onDetails={setDetails}
        silences={silences}
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {server.awaiting_agent && !server.revoked_at && <WaitingForAgent server={server} />}
      {server.unreachable && !server.revoked_at && <UnreachableBanner name={server.hostname} note={server.unreachable} />}
      {server.revoked_at && (
        <RemovedPanel
          kind="agent"
          name={server.hostname}
          osFamily={server.os_family}
          removedAt={server.revoked_at}
          canAdminister={!device && administersCustomer(me, server.customer_id)}
        />
      )}
      {!server.revoked_at && (
        <VirtualizationSection
          server={server}
          hypervWatched={"sensors" in sensors && sensors.sensors.some((sensor) => sensor.kind === "hyperv")}
        />
      )}
      <Tabs
        label={t("server.sections")}
        idBase="server"
        selected={shown}
        // The logs' filters and time belong to that tab.
        onSelect={(next) =>
          setParams({
            tab: next === "sensors" ? null : next,
            level: null,
            unit: null,
            q: null,
            from: null,
            to: null,
            period: null,
            by: null,
            address: null,
          })
        }
        tabs={[
          {
            id: "sensors",
            label: t("server.tab.sensors"),
            extra: "sensors" in sensors ? t("server.tab.count", { count: rows.length }) : undefined,
          },
          { id: "dependencies" as const, label: t("dependencies.tab") },
          // A device has no agent: no logs, updates or commands (ADR-0148).
          ...(device
            ? [{ id: "flows" as const, label: t("flows.tab") }]
            : [
                { id: "logs" as const, label: t("server.tab.logs") },
                { id: "updates" as const, label: t("server.tab.updates") },
                {
                  id: "commands" as const,
                  label: t("server.tab.commands"),
                  extra: active > 0 ? t("server.tab.running", { count: active }) : undefined,
                },
                ...(terminal ? [{ id: "terminal" as const, label: t("remote.tab") }] : []),
                ...(desktop ? [{ id: "desktop" as const, label: t("remote.desktop.tab") }] : []),
                { id: "inventory" as const, label: t("inventory.tab") },
              ]),
        ]}
      />
      <TabPanel idBase="server" id={shown}>
        {shown === "dependencies" && <DependenciesTab server={server} checks={checkNames} />}
        {shown === "flows" && <FlowsTab server={server} />}
        {shown === "sensors" && (
          <ServerSensorsTab
            server={server}
            sensors={sensors}
            rows={rows}
            window={data.window}
            rules={data.rules}
            incidentsFailed={incidents === null}
            onChanged={refresh}
            checkSilences={silences.checkSilences}
            onSilenceCheck={silences.silenceCheck}
            onPauseCheck={silences.pauseCheck}
            onPaused={silences.refreshSilences}
          />
        )}
        {!device && tab === "logs" && <ServerLogsTab server={server} />}
        {!device && tab === "updates" && <ServerUpdatesTab server={server} />}
        {!device && tab === "inventory" && <InventoryTab server={server} />}
        {!device && tab === "commands" && !server.revoked_at && aiCommands(server) === "off" && <AllowOnThisServer server={server} onOpenDetails={() => {
          setDetails(true);
          window.scrollTo({ top: 0 });
        }} />}
        {!device && tab === "commands" && (
          <ServerCommandsTab
            serverId={server.id}
            commands={commands}
            now={updatedAt}
            onLoadMore={() => setCommandLimit((n) => Math.min(MAX_LIMIT, n + COMMANDS_PAGE))}
            canLoadMore={commandLimit < MAX_LIMIT}
          />
        )}
        {shown === "terminal" && <ServerTerminalTab server={server} />}
        {shown === "desktop" && <ServerDesktopTab server={server} />}
      </TabPanel>
    </Page>
  );
}

/**
 * Title, status and actions (Update agent, Silence ▾, ⋯), one line of facts
 * with a Details disclosure for the rest, and a badge only for what is not in
 * its default, healthy state: AI commands on, offline alerts off, rollback
 * limited, a certificate problem, no package updates.
 */
function ServerHeader({
  server,
  latestAgent,
  commands,
  now,
  stale,
  onChanged,
  details,
  onDetails: setDetails,
  silences,
}: {
  server: ServerDetail;
  latestAgent: string | null;
  commands: ServerData["commands"];
  now: Date;
  stale: boolean;
  onChanged: () => void;
  details: boolean;
  onDetails: (open: boolean) => void;
  silences: SilenceBarParts;
}) {
  const me = useMe();
  const severalSites = multiSite(me);
  const [removing, setRemoving] = useState(false);
  const live = !server.revoked_at;
  const canRemove = live && administersCustomer(me, server.customer_id);
  const device = server.kind === "device";
  const canEditDevice = device && live && modifiesSite(me, server.customer_id, server.site_id);
  const [editing, setEditing] = useState(false);
  return (
    <>
      <PageHeader
        detail
        title={server.hostname}
        status={<StatusLabel kind="server" value={server.revoked_at ? "revoked" : server.awaiting_agent ? "waiting" : server.unreachable ? "unreachable" : server.status} />}
        meta={<RefreshLine updatedAt={now} stale={stale} />}
        actions={
          <>
            <UpdateAgent server={server} latest={latestAgent} commands={commands} onQueued={onChanged} />
            {live && silences.menu}
            {(canRemove || canEditDevice) && (
              <RowMenu
                label={t("server.more", { name: server.hostname })}
                items={[
                  ...(canEditDevice ? [{ label: t("devices.edit.trigger"), onSelect: () => setEditing(true) }] : []),
                  ...(canRemove
                    ? [{ label: t("server.remove.trigger", { name: server.hostname }), onSelect: () => setRemoving(true), danger: true }]
                    : []),
                ]}
              />
            )}
          </>
        }
        sub={
          <>
            <span className={styles.line}>
              {device ? (
                <>
                  {server.address}
                  {severalSites && (
                    <>
                      {" · "}
                      <Link href={siteHref(server.site_id)}>{server.site_name}</Link>
                    </>
                  )}
                </>
              ) : (
              <>
              {[
                formatOs(server.os_family, server.os_version),
                t("server.line.agent", { version: server.agent_version ?? t("server.fact.unknown") }),
              ].join(" · ")}
              {" · "}
              {severalSites && (
                <>
                  <Link href={siteHref(server.site_id)}>{server.site_name}</Link>
                  {" · "}
                </>
              )}
              {server.last_heartbeat ? (
                <>
                  {t("server.line.seen")} <RelativeTime value={server.last_heartbeat} now={now} stale={stale} />
                </>
              ) : (
                t("server.line.neverSeen")
              )}
              </>
              )}
            </span>
            {live && !server.awaiting_agent && !device && <ServerBadges server={server} now={now} />}
            <Button variant="ghost" size="small" aria-expanded={details} onClick={() => setDetails(!details)}>
              {t("ui.details")}
            </Button>
          </>
        }
      />
      {removing && canRemove && (
        <RemovePeer kind="servers" id={server.id} name={server.hostname} device={device} onRemoved={onChanged} onClose={() => setRemoving(false)} />
      )}
      {editing && canEditDevice && (
        <EditDevice
          server={server}
          onClose={() => setEditing(false)}
          onSaved={() => {
            setEditing(false);
            onChanged();
          }}
        />
      )}
      {live && silences.notices}
      {live && silences.form}
      {details &&
        (device ? (
          <DeviceFacts server={server} onChanged={onChanged} />
        ) : (
          <ServerFacts server={server} latestAgent={latestAgent} now={now} onChanged={onChanged} />
        ))}
    </>
  );
}

/** Badges for the states that are not the default; nothing when all is as usual. */
function ServerBadges({ server, now }: { server: ServerDetail; now: Date }) {
  const { certificate } = server;
  const problem = !certificate
    ? null
    : certificate.revoked
      ? statusWord("certificate", "revoked")[0]
      : certificateProblem(certificate.not_after, now);
  return (
    <>
      {aiCommands(server) === "accepted" && (
        <Hint tone="info" label={t("server.badge.aiOn")}>
          {t("server.ai.runs")}
        </Hint>
      )}
      {(server.offline_alerts === false || server.site_offline_alerts === false) && (
        <Hint tone="neutral" label={t("server.badge.offlineOff")}>
          {t("server.offline.none")}
        </Hint>
      )}
      <RollbackBadge serverId={server.id} />
      {problem && (
        <Hint
          tone={certificate!.revoked || certificateState(certificate!.not_after, now) === "failed" ? "bad" : "warn"}
          label={t("server.fact.certificate")}
        >
          {problem}
        </Hint>
      )}
      {server.package_updates === false && (
        <Hint tone="neutral" label={t("server.badge.noPackageUpdates")}>
          {t("server.fact.noPackageUpdates")}
        </Hint>
      )}
    </>
  );
}

function ServerHeading({ server }: { server: ServerDetail }) {
  const scope = useScope();
  return (
    <Breadcrumb
      parents={[{ label: t("server.fleet"), href: scopedHref("/", scope) }]}
      siblings={serverSiblings(server, scope)}
    />
  );
}

/**
 * "Update agent to 0.3.0…", for administrators of the server's customer
 * when the server holds a newer release (ADR-0053). Hidden while an
 * update is queued or running; the Commands tab follows it.
 */
function UpdateAgent({
  server,
  latest,
  commands,
  onQueued,
}: {
  server: ServerDetail;
  latest: string | null;
  commands: ServerData["commands"];
  onQueued: () => void;
}) {
  const me = useMe();
  const pending = "commands" in commands && commands.commands.some((c) => c.kind === "self_update" && isActive(c));
  if (
    server.revoked_at ||
    pending ||
    !latest ||
    !updateAvailable(server.agent_version, latest) ||
    !administersCustomer(me, server.customer_id)
  ) {
    return null;
  }
  return (
    <InlineConfirm
      trigger={t("server.update.trigger", { version: latest })}
      triggerVariant="default"
      title={t("server.update.title", { server: server.hostname, version: latest })}
      consequence={t("server.update.consequence", {
        version: latest,
        current: server.agent_version ?? t("server.update.current"),
      })}
      confirmLabel={t("server.update.confirm", { server: server.hostname })}
      pendingLabel={t("server.update.queuing")}
      onConfirm={async () => {
        await api.updateAgent(server.id, latest);
        onQueued();
      }}
      describeError={(e) => t("server.update.failed", { error: describeError(e) })}
    />
  );
}

/** How to turn on AI commands on this server, or who can. */
function AllowOnThisServer({ server, onOpenDetails }: { server: ServerDetail; onOpenDetails: () => void }) {
  const me = useMe();
  const where = server.os_family === "windows" ? t("server.allow.windows") : "";
  if (aiSwitch(server) === "both") {
    return (
      <Banner tone="warn">
        <strong>{t("server.allow.off", { server: server.hostname })}</strong> {t("server.allow.refused")}{" "}
        {administersSite(me, server.customer_id, server.site_id) ? (
          <Button variant="ghost" size="small" onClick={onOpenDetails}>
            {t("server.allow.openDetails")}
          </Button>
        ) : (
          t("server.allow.askSite")
        )}
      </Banner>
    );
  }
  return (
    <Banner tone="warn">
      <strong>{t("server.allow.off", { server: server.hostname })}</strong>{" "}
      {administersSite(me, server.customer_id, server.site_id) ? (
        <>
          {t("server.allow.onlyOnServer", { where })}
          <AllowCommandsLine osFamily={server.os_family} fromWeb />
          {t("server.allow.forGood")}
          <AllowCommandsLine osFamily={server.os_family} />
        </>
      ) : (
        t("server.allow.askAdmin")
      )}
    </Banner>
  );
}

/** Whether the server runs AI's commands, in words, with the way to change it. */
function AiCommandsFact({ server, onChanged }: { server: ServerDetail; onChanged: () => void }) {
  const scope = useScope();
  const state = aiCommands(server);
  const lastContact = server.status === "offline" && t("server.ai.lastContact");
  switch (state) {
    case "accepted":
      return (
        <>
          <Label tone="ok">{t("server.ai.on")}</Label>
          {lastContact && <> {lastContact}</>}
          <AiCommandsChangeLine server={server} />
          <AiCommandsSwitch server={server} on={false} onChanged={onChanged} />
        </>
      );
    case "off":
      return (
        <>
          <Label tone="warn">{t("server.ai.off")}</Label>
          {lastContact && <> {lastContact}</>}
          <AiCommandsChangeLine server={server} />
          {aiSwitch(server) === "both" ? (
            <AiCommandsSwitch server={server} on onChanged={onChanged} />
          ) : (
            <>
              {" "}
              {t("server.ai.onlyOnServer")}{" "}
              <Link href={scopedHref(serverHref(server.id, "commands"), scope)}>{t("server.ai.how")}</Link>
            </>
          )}
        </>
      );
    case "unsupported":
      return (
        <>
          <Label tone="neutral">{t("server.ai.notAvailable")}</Label> {unsupportedReason(server.os_family)}
        </>
      );
    case "unknown":
      return (
        <>
          <Label tone="neutral">{t("server.ai.notKnown")}</Label>{" "}
          {server.via_relay ? t("server.ai.relayWillSay") : t("server.ai.agentWillSay")}
        </>
      );
  }
}

/**
 * " Turned on by alice at 14:02.", " Turning on… asked by alice at 14:02.", or why the last request
 * did not apply: who changed AI commands from the web interface last (ADR-0098).
 */
function AiCommandsChangeLine({ server }: { server: ServerDetail }) {
  const change = server.ai_commands_change;
  if (!change) return null;
  const who = change.requested_by ?? t("server.change.removedUser");
  const at = minutes(change.issued_at);
  if (isActive(change)) {
    const later = server.status === "offline" ? t("server.change.whenBack") : "";
    return <>{t(change.on ? "server.change.turningOn" : "server.change.turningOff", { who, at }) + later}</>;
  }
  if (change.state === "succeeded") {
    // A later change on the server itself makes the request history, not the reason.
    if (change.on !== (aiCommands(server) === "accepted")) return null;
    const done = minutes(change.finished_at ?? change.issued_at);
    return <>{t(change.on ? "server.change.turnedOn" : "server.change.turnedOff", { who, at: done })}</>;
  }
  if (change.state === "failed") {
    const error = change.error ?? t("server.change.unknownError");
    return <>{t(change.on ? "server.change.failedOn" : "server.change.failedOff", { who, at, error })}</>;
  }
  if (change.state === "expired") {
    return <>{t(change.on ? "server.change.expiredOn" : "server.change.expiredOff", { who, at })}</>;
  }
  return null;
}

/** "Turn on…" or "Turn off…" for administrators of the server's site, unless a request is under way. */
function AiCommandsSwitch({ server, on, onChanged }: { server: ServerDetail; on: boolean; onChanged: () => void }) {
  const me = useMe();
  const change = server.ai_commands_change;
  if (
    server.revoked_at ||
    aiSwitch(server) === "none" ||
    (change && isActive(change)) ||
    !administersSite(me, server.customer_id, server.site_id)
  ) {
    return null;
  }
  return (
    <>
      {" "}
      <InlineConfirm
        trigger={on ? t("server.switch.onTrigger") : t("server.switch.offTrigger")}
        triggerVariant={on ? "default" : "danger"}
        layout="row"
        title={t(on ? "server.switch.onTitle" : "server.switch.offTitle", { server: server.hostname })}
        consequence={on ? t("server.switch.onConsequence") : t("server.switch.offConsequence")}
        confirmLabel={on ? t("server.switch.onConfirm") : t("server.switch.offConfirm")}
        confirmVariant={on ? "k" : "danger"}
        pendingLabel={t("server.switch.sending")}
        onConfirm={async () => {
          await api.switchAiCommands(server.id, on);
          onChanged();
        }}
        describeError={(e) => t("server.switch.failed", { error: describeError(e) })}
      />
    </>
  );
}

/** "2026-10-19 02:14". */
const minutes = (iso: string) => formatExact(new Date(iso)).slice(0, 16);

function ServerFacts({
  server,
  latestAgent,
  now,
  onChanged,
}: {
  server: ServerDetail;
  latestAgent: string | null;
  now: Date;
  onChanged: () => void;
}) {
  const { certificate } = server;
  const summary = commandSummary(server);
  const me = useMe();
  const canMap = administersCustomer(me, server.customer_id);
  const rollback = useRollback(server.id, !server.revoked_at);
  // The header says the system, the agent's version, the site and when it was last seen.
  const agentNews = [
    updateAvailable(server.agent_version, latestAgent) && t("server.fact.available", { version: latestAgent ?? "" }),
    server.via_relay && t("server.fact.via", { relay: server.via_relay }),
  ].filter(Boolean);
  return (
    <FactsBox
      columns={2}
      facts={[
        ...(multiCustomer(me) ? [{ label: t("server.fact.customer"), value: server.customer_name }] : []),
        {
          label: server.connected_at ? t("server.fact.connectedSince") : t("server.fact.connected"),
          value: server.connected_at ? minutes(server.connected_at) : t("server.fact.notNow"),
        },
        ...(agentNews.length > 0 ? [{ label: t("server.fact.agent"), value: agentNews.join(" · ") }] : []),
        {
          label: t("server.fact.certificate"),
          value: certificate ? <CertificateFact certificate={certificate} now={now} /> : t("server.fact.noCertificate"),
        },
        ...(server.package_updates === false
          ? [{ label: t("server.fact.packageUpdates"), value: t("server.fact.noPackageUpdates") }]
          : []),
        { label: t("server.fact.added"), value: minutes(server.created_at) },
        ...(server.revoked_at
          ? []
          : [
              { label: t("server.fact.aiCommands"), value: <AiCommandsFact server={server} onChanged={onChanged} /> },
              { label: t("server.fact.offlineAlerts"), value: <OfflineAlerts kind="servers" target={server} onChanged={onChanged} /> },
              // What a change gets first; the Proxmox mapping for administrators of the customer.
              // A limited rollback without a mapping to change is the header's badge alone.
              ...(rollback.data?.snapshots.available || canMap
                ? [{ label: t("server.fact.rollback"), value: <RollbackFact server={server} canMap={canMap} /> }]
                : []),
            ]),
        ...(summary ? [{ label: t("server.fact.commands"), value: summary }] : []),
        // Usually the host name, then said already.
        ...(server.peer_id !== server.hostname ? [{ label: t("server.fact.certificateId"), value: server.peer_id, mono: true }] : []),
      ]}
    />
  );
}

function CertificateFact({ certificate, now }: { certificate: NonNullable<ServerDetail["certificate"]>; now: Date }) {
  const date = formatExact(new Date(certificate.not_after)).slice(0, 10);
  if (certificate.revoked) {
    return (
      <>
        <StatusLabel kind="certificate" value="revoked" />{" "}
        {certificate.revoked_at && minutes(certificate.revoked_at)}
        {certificate.revocation_reason && ` (${certificate.revocation_reason.replaceAll("_", " ")})`}
      </>
    );
  }
  const problem = certificateProblem(certificate.not_after, now);
  if (!problem) return <>{t("server.fact.certificateValid", { date })}</>;
  return (
    <>
      <Label tone={certificateState(certificate.not_after, now) === "failed" ? "bad" : "warn"}>{problem}</Label> {date}
    </>
  );
}
