"use client";

import { useSearchParams } from "next/navigation";
import Link from "next/link";
import { useCallback, useEffect, useMemo, useState } from "react";

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
import { administersCustomer, administersSite, modifiesSite, multiCustomer, useMe } from "@/lib/me";
import { serverHref, siteHref } from "@/lib/routes";
import { scopedHref, useScope } from "@/lib/scope";
import { type SensorValues, primaryMetric, sensorRows } from "@/lib/sensors";
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
import { MonitoringSettings } from "./MonitoringSettings";
import { OfflineAlerts } from "./OfflineAlerts";
import { RemovedPanel, RemovePeer } from "./RemovePeer";
import { ServerCommandsTab } from "./ServerCommandsTab";
import { RollbackFact } from "./ServerRollback";
import { ServerLogsTab } from "./ServerLogsTab";
import { ServerSensorsTab } from "./ServerSensorsTab";
import { ServerUpdatesTab } from "./ServerUpdatesTab";
import { SilenceBar } from "./Silences";

/** Commands loaded at first; "Load more" adds pages of this size. */
export const COMMANDS_PAGE = 50;
/** The API's largest page, which caps what a refresh reloads. */
const MAX_LIMIT = 500;

const TABS = ["sensors", "alerts", "logs", "updates", "commands"] as const;
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
  /** The latest value of each sensor's primary metric. */
  values: Map<EffectiveSensor, SensorValues>;
  /** Null when the incidents could not be read. */
  incidents: Incident[] | null;
  commands: { commands: Command[]; nextCursor: string | null } | { error: string };
  /** The newest agent release the server holds (ADR-0053); null when none or unreadable. */
  latestAgent: string | null;
}

/** `promise`, or `fallback(error)` when it fails for another reason than an abort. */
function settle<T, F>(promise: Promise<T>, fallback: (error: unknown) => F): Promise<T | F> {
  return promise.catch((e: unknown) => {
    if (isAbortError(e)) throw e;
    return fallback(e);
  });
}

/** For a check slower than every minute, the last value within two of its intervals (ADR-0097). */
const lookback = (sensor: EffectiveSensor) =>
  sensor.interval_ms > 60_000 ? { lookbackS: Math.ceil((2 * sensor.interval_ms) / 1000) } : {};

async function loadServer(id: string, commandLimit: number, signal: AbortSignal): Promise<ServerData> {
  const sensors = settle(api.serverSensors(id, signal), (e) => ({ error: describeError(e) }));
  const values = sensors.then(async (config) => {
    const list = "sensors" in config ? config.sensors : [];
    const results = await Promise.all(
      list.map((sensor) =>
        settle(
          api.queryInstant({ serverId: id, ...primaryMetric(sensor), ...lookback(sensor) }, signal).then(({ series }) => ({ series })),
          (e) => ({ error: metricsError(e) }),
        ),
      ),
    );
    return new Map(list.map((sensor, i) => [sensor, results[i]!]));
  });
  const [server, config, latest, incidents, commands, releases] = await Promise.all([
    api.server(id, signal),
    sensors,
    values,
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
  ]);
  return { server, sensors: config, values: latest, incidents, commands, latestAgent: releases?.latest ?? null };
}

export function ServerPage({ id }: { id: string | null }) {
  const me = useMe();
  const valid = id !== null && UUID.test(id);
  const [params, setParams] = useUrlParams();
  const tab: ServerTab = TABS.find((t) => t === params.get("tab")) ?? "sensors";
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
        title="Server not found"
        actions={<ButtonLink href="/">Back to the fleet</ButtonLink>}
      />
    );
  }
  if (!data || !updatedAt) {
    return error !== null && !isUnauthorized(error) ? (
      <ErrorState what="the server" error={describeError(error)} onRetry={refresh} />
    ) : (
      <LoadingBlock label="Loading server" />
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
        label="Server sections"
        idBase="server"
        selected={tab}
        // The logs' filters and time belong to that tab.
        onSelect={(next) =>
          setParams({ tab: next === "sensors" ? null : next, level: null, unit: null, q: null, from: null, to: null })
        }
        tabs={[
          { id: "sensors", label: "Sensors", extra: "sensors" in sensors ? `· ${rows.length}` : undefined },
          { id: "alerts", label: "Alerts and checks" },
          { id: "logs", label: "Logs" },
          { id: "updates", label: "Updates" },
          { id: "commands", label: "Commands", extra: active > 0 ? `· ${active} running` : undefined },
        ]}
      />
      <TabPanel idBase="server" id={tab}>
        {tab === "sensors" && (
          <ServerSensorsTab server={server} sensors={sensors} rows={rows} incidentsFailed={incidents === null} now={updatedAt} />
        )}
        {tab === "alerts" && <ServerAlertsTab server={server} />}
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

/** The server's alerts and checks, editable by operators of its site. */
function ServerAlertsTab({ server }: { server: ServerDetail }) {
  const me = useMe();
  const { id, site_id: siteId, site_name: siteName, os_family: osFamily } = server;
  const target = useMemo(() => ({ kind: "server" as const, id, siteId, siteName, osFamily }), [id, siteId, siteName, osFamily]);
  return <MonitoringSettings target={target} canEdit={!server.revoked_at && modifiesSite(me, server.customer_id, siteId)} />;
}

function ServerHeading({ server }: { server: ServerDetail }) {
  const scope = useScope();
  return <Breadcrumb parents={[{ label: "Fleet", href: scopedHref("/", scope) }]} current={server.hostname} />;
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
      trigger={`Update agent to ${latest}…`}
      triggerVariant="default"
      title={`Update the agent of ${server.hostname} to ${latest}?`}
      consequence={`The agent downloads ${latest}, checks its signature and restarts into it. If the new agent does not connect within 5 minutes, ${server.agent_version ?? "the current version"} comes back. Follow it in the Commands tab.`}
      confirmLabel={`Update ${server.hostname}`}
      pendingLabel="Queuing…"
      onConfirm={async () => {
        await api.updateAgent(server.id, latest);
        onQueued();
      }}
      describeError={(e) => `The update was not queued. ${describeError(e)}`}
    />
  );
}

/** How to turn on AI commands on this server, or who can. */
function AllowOnThisServer({ server }: { server: ServerDetail }) {
  const me = useMe();
  const where = server.os_family === "windows" ? ", in PowerShell as an administrator" : "";
  if (aiSwitch(server) === "both") {
    return (
      <Banner tone="warn">
        <strong>AI commands are off on {server.hostname}.</strong> AI&apos;s commands are refused here and its incidents
        go to a person.{" "}
        {administersSite(me, server.customer_id, server.site_id)
          ? "Turn them on from AI commands, above."
          : "An administrator of its site can turn them on from this page."}
      </Banner>
    );
  }
  return (
    <Banner tone="warn">
      <strong>AI commands are off on {server.hostname}.</strong>{" "}
      {administersSite(me, server.customer_id, server.site_id) ? (
        <>
          AI&apos;s commands are refused here and its incidents go to a person. They can only be turned on on the server.
          To let administrators turn them on and off from this page, run this once on the server{where}. This page
          changes by itself when it is done.
          <AllowCommandsLine osFamily={server.os_family} fromWeb />
          Or, to turn them on for good from the server itself:
          <AllowCommandsLine osFamily={server.os_family} />
        </>
      ) : (
        "AI's commands are refused here and its incidents go to a person. An administrator can turn them on on the server."
      )}
    </Banner>
  );
}

/** Whether the server runs AI's commands, in words, with the way to change it. */
function AiCommandsFact({ server, onChanged }: { server: ServerDetail; onChanged: () => void }) {
  const scope = useScope();
  const state = aiCommands(server);
  const lastContact = server.status === "offline" && " As of its last contact.";
  switch (state) {
    case "accepted":
      return (
        <>
          <Label tone="ok">On</Label> Runs the commands AI sends.{lastContact}
          <AiCommandsChangeLine server={server} />
          <AiCommandsSwitch server={server} on={false} onChanged={onChanged} />
        </>
      );
    case "off":
      return (
        <>
          <Label tone="warn">Off</Label> Refuses AI&apos;s commands.{lastContact}
          <AiCommandsChangeLine server={server} />
          {aiSwitch(server) === "both" ? (
            <AiCommandsSwitch server={server} on onChanged={onChanged} />
          ) : (
            <>
              {" "}
              Can only be turned on on the server:{" "}
              <Link href={scopedHref(serverHref(server.id, "commands"), scope)}>how</Link>
            </>
          )}
        </>
      );
    case "unsupported":
      return (
        <>
          <Label tone="neutral">Not available</Label> {unsupportedReason(server.os_family)}
        </>
      );
    case "unknown":
      return (
        <>
          <Label tone="neutral">Not known yet</Label>{" "}
          {server.via_relay ? "The agent and its relay" : "The agent"} will say once updated, which happens by itself.
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
  const word = change.on ? "on" : "off";
  const who = change.requested_by ?? "a removed user";
  const asked = `asked by ${who} at ${minutes(change.issued_at)}`;
  if (isActive(change)) {
    const later = server.status === "offline" ? " It applies when the server is back." : "";
    return <> Turning {word}… {asked}.{later}</>;
  }
  if (change.state === "succeeded") {
    // A later change on the server itself makes the request history, not the reason.
    if (change.on !== (aiCommands(server) === "accepted")) return null;
    return <> Turned {word} by {who} at {minutes(change.finished_at ?? change.issued_at)}.</>;
  }
  if (change.state === "failed") {
    return <> The request to turn them {word}, {asked}, failed: {change.error ?? "unknown error"}.</>;
  }
  if (change.state === "expired") {
    return <> The request to turn them {word}, {asked}, expired before the server was back.</>;
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
  const word = on ? "on" : "off";
  return (
    <>
      {" "}
      <InlineConfirm
        trigger={`Turn ${word}…`}
        triggerVariant={on ? "default" : "danger"}
        layout="row"
        title={`Turn AI commands ${word} on ${server.hostname}?`}
        consequence={
          on
            ? "AI runs read-only commands here at once, and changes once an approver accepts them. The channels covering this server are told."
            : "AI's commands are refused here and its incidents go to a person. The channels covering this server are told."
        }
        confirmLabel={`Turn ${word}`}
        confirmVariant={on ? "k" : "danger"}
        pendingLabel="Sending…"
        onConfirm={async () => {
          await api.switchAiCommands(server.id, on);
          onChanged();
        }}
        describeError={(e) => `Nothing changed. ${describeError(e)}`}
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
          label: "Site",
          value: (
            <>
              <Link href={siteHref(server.site_id)}>{server.site_name}</Link>
              {multiCustomer(me) && ` · ${server.customer_name}`}
            </>
          ),
        },
        {
          label: "Last seen",
          value: server.last_heartbeat ? <RelativeTime value={server.last_heartbeat} now={now} stale={stale} /> : "never",
        },
        { label: "OS", value: formatOs(server.os_family, server.os_version) },
        {
          label: server.connected_at ? "Connected since" : "Connected",
          value: server.connected_at ? minutes(server.connected_at) : "not now",
        },
        {
          label: "Agent",
          value: [
            server.agent_version ?? "unknown",
            updateAvailable(server.agent_version, latestAgent) && `${latestAgent} available`,
            server.via_relay && `via ${server.via_relay}`,
          ]
            .filter(Boolean)
            .join(" · "),
        },
        { label: "Certificate", value: certificate ? <CertificateFact certificate={certificate} now={now} /> : "none issued" },
        ...(server.package_updates === false
          ? [{ label: "Package updates", value: "Package updates aren't available on this system; monitoring works normally" }]
          : []),
        { label: "Added", value: minutes(server.created_at) },
        ...(server.revoked_at
          ? []
          : [
              { label: "AI commands", value: <AiCommandsFact server={server} onChanged={onChanged} /> },
              { label: "Offline alerts", value: <OfflineAlerts kind="servers" target={server} onChanged={onChanged} /> },
              // What a change gets first; the Proxmox mapping for administrators of the customer.
              {
                label: "Rollback",
                value: <RollbackFact server={server} canMap={administersCustomer(me, server.customer_id)} />,
              },
            ]),
      ]}
      details={[
        ...(summary ? [{ label: "Commands", value: summary }] : []),
        { label: "Certificate ID", value: server.peer_id, mono: true },
        ...(certificate ? [{ label: "Certificate valid until", value: formatExact(new Date(certificate.not_after)).slice(0, 10) }] : []),
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
  if (!problem) return <>Valid, renews automatically</>;
  return (
    <>
      <Label tone={certificateState(certificate.not_after, now) === "failed" ? "bad" : "warn"}>{problem}</Label> {date}
    </>
  );
}
