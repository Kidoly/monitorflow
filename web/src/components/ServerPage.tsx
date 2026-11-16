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
import { administersCustomer, administersSite, multiCustomer, useMe } from "@/lib/me";
import { serverHref, siteHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { type SensorValues, sensorRows, valuesBySensor } from "@/lib/sensors";
import { usePolledData } from "@/lib/usePolledData";
import { useUrlParams } from "@/lib/urlState";
import { UUID } from "@/lib/uuid";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { ButtonLink } from "@/components/ui/Button";
import { FactsBox } from "@/components/ui/FactsBox";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { Page } from "@/components/ui/Page";
import { Banner, ErrorState, LoadingBlock, NotFound, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { TabPanel, Tabs } from "@/components/ui/Tabs";
import { formatExact } from "@/components/ui/time";

import { AllowCommandsLine } from "./AllowCommands";
import { serverSiblings } from "./breadcrumbSiblings";
import { OfflineAlerts } from "./OfflineAlerts";
import { RemovedPanel, RemovePeer } from "./RemovePeer";
import { ServerCommandsTab } from "./ServerCommandsTab";
import { RollbackFact } from "./ServerRollback";
import { ServerLogsTab } from "./ServerLogsTab";
import { type SensorRules, ServerSensorsTab } from "./ServerSensorsTab";
import { ServerUpdatesTab } from "./ServerUpdatesTab";
import { SilenceBar } from "./Silences";

/** Commands loaded at first; "Load more" adds pages of this size. */
export const COMMANDS_PAGE = 50;
/** The API's largest page, which caps what a refresh reloads. */
const MAX_LIMIT = 500;

const TABS = ["sensors", "logs", "updates", "commands"] as const;
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
  const fetcher = useCallback((signal: AbortSignal) => loadServer(id ?? "", commandLimit, signal), [id, commandLimit]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });

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
  const rows = "sensors" in sensors ? sensorRows(sensors.sensors, values, incidents ?? []) : [];
  const active = "commands" in commands ? commands.commands.filter(isActive).length : 0;

  return (
    <Page>
      <ServerHeading server={server} />
      <PageHeader
        detail
        title={server.hostname}
        status={<StatusLabel kind="server" value={server.revoked_at ? "revoked" : server.status} />}
        meta={<RefreshLine updatedAt={updatedAt} stale={stale} />}
        actions={
          <>
            <UpdateAgent server={server} latest={latestAgent} commands={commands} onQueued={refresh} />
            {!server.revoked_at && administersCustomer(me, server.customer_id) && (
              <RemovePeer kind="servers" id={server.id} name={server.hostname} onRemoved={refresh} />
            )}
          </>
        }
      />
      {stale && <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />}
      {server.revoked_at && (
        <RemovedPanel
          kind="agent"
          name={server.hostname}
          osFamily={server.os_family}
          removedAt={server.revoked_at}
          canAdminister={administersCustomer(me, server.customer_id)}
        />
      )}
      {!server.revoked_at && (
        <SilenceBar
          target={{ scope: "server", id: server.id, name: server.hostname, customerId: server.customer_id, siteId: server.site_id }}
        />
      )}
      <ServerFacts server={server} latestAgent={latestAgent} now={updatedAt} stale={stale} onChanged={refresh} />
      <Tabs
        label={t("server.sections")}
        idBase="server"
        selected={tab}
        // The logs' filters and time belong to that tab.
        onSelect={(next) =>
          setParams({ tab: next === "sensors" ? null : next, level: null, unit: null, q: null, from: null, to: null })
        }
        tabs={[
          {
            id: "sensors",
            label: t("server.tab.sensors"),
            extra: "sensors" in sensors ? t("server.tab.count", { count: rows.length }) : undefined,
          },
          { id: "logs", label: t("server.tab.logs") },
          { id: "updates", label: t("server.tab.updates") },
          {
            id: "commands",
            label: t("server.tab.commands"),
            extra: active > 0 ? t("server.tab.running", { count: active }) : undefined,
          },
        ]}
      />
      <TabPanel idBase="server" id={tab}>
        {tab === "sensors" && (
          <ServerSensorsTab
            server={server}
            sensors={sensors}
            rows={rows}
            window={data.window}
            rules={data.rules}
            incidentsFailed={incidents === null}
            now={updatedAt}
            onChanged={refresh}
          />
        )}
        {tab === "logs" && <ServerLogsTab server={server} />}
        {tab === "updates" && <ServerUpdatesTab server={server} />}
        {tab === "commands" && !server.revoked_at && aiCommands(server) === "off" && <AllowOnThisServer server={server} />}
        {tab === "commands" && (
          <ServerCommandsTab
            serverId={server.id}
            commands={commands}
            now={updatedAt}
            onLoadMore={() => setCommandLimit((n) => Math.min(MAX_LIMIT, n + COMMANDS_PAGE))}
            canLoadMore={commandLimit < MAX_LIMIT}
          />
        )}
      </TabPanel>
    </Page>
  );
}

function ServerHeading({ server }: { server: ServerDetail }) {
  const scope = useScope();
  return (
    <Breadcrumb
      parents={[{ label: t("server.fleet"), href: scopedHref("/", scope) }]}
      current={server.hostname}
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
function AllowOnThisServer({ server }: { server: ServerDetail }) {
  const me = useMe();
  const where = server.os_family === "windows" ? t("server.allow.windows") : "";
  if (aiSwitch(server) === "both") {
    return (
      <Banner tone="warn">
        <strong>{t("server.allow.off", { server: server.hostname })}</strong> {t("server.allow.refused")}{" "}
        {administersSite(me, server.customer_id, server.site_id)
          ? t("server.allow.turnOnAbove")
          : t("server.allow.askSite")}
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
          <Label tone="ok">{t("server.ai.on")}</Label> {t("server.ai.runs")}
          {lastContact}
          <AiCommandsChangeLine server={server} />
          <AiCommandsSwitch server={server} on={false} onChanged={onChanged} />
        </>
      );
    case "off":
      return (
        <>
          <Label tone="warn">{t("server.ai.off")}</Label> {t("server.ai.refuses")}
          {lastContact}
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
  stale,
  onChanged,
}: {
  server: ServerDetail;
  latestAgent: string | null;
  now: Date;
  stale: boolean;
  onChanged: () => void;
}) {
  const { certificate } = server;
  const summary = commandSummary(server);
  const me = useMe();
  return (
    <FactsBox
      columns={2}
      facts={[
        {
          label: t("server.fact.site"),
          value: (
            <>
              <Link href={siteHref(server.site_id)}>{server.site_name}</Link>
              {multiCustomer(me) && ` · ${server.customer_name}`}
            </>
          ),
        },
        {
          label: t("server.fact.lastSeen"),
          value: server.last_heartbeat ? (
            <RelativeTime value={server.last_heartbeat} now={now} stale={stale} />
          ) : (
            t("server.fact.never")
          ),
        },
        { label: t("server.fact.os"), value: formatOs(server.os_family, server.os_version) },
        {
          label: server.connected_at ? t("server.fact.connectedSince") : t("server.fact.connected"),
          value: server.connected_at ? minutes(server.connected_at) : t("server.fact.notNow"),
        },
        {
          label: t("server.fact.agent"),
          value: [
            server.agent_version ?? t("server.fact.unknown"),
            updateAvailable(server.agent_version, latestAgent) && t("server.fact.available", { version: latestAgent ?? "" }),
            server.via_relay && t("server.fact.via", { relay: server.via_relay }),
          ]
            .filter(Boolean)
            .join(" · "),
        },
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
              {
                label: t("server.fact.rollback"),
                value: <RollbackFact server={server} canMap={administersCustomer(me, server.customer_id)} />,
              },
            ]),
      ]}
      details={[
        ...(summary ? [{ label: t("server.fact.commands"), value: summary }] : []),
        { label: t("server.fact.certificateId"), value: server.peer_id, mono: true },
        ...(certificate ? [{ label: t("server.fact.certificateUntil"), value: formatExact(new Date(certificate.not_after)).slice(0, 10) }] : []),
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
  if (!problem) return <>{t("server.fact.certificateValid")}</>;
  return (
    <>
      <Label tone={certificateState(certificate.not_after, now) === "failed" ? "bad" : "warn"}>{problem}</Label> {date}
    </>
  );
}
