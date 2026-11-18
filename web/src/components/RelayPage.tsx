"use client";

import { useSearchParams } from "next/navigation";
import { useCallback, useState } from "react";

import { Button, ButtonLink } from "@/components/ui/Button";
import { Breadcrumb } from "@/components/ui/Breadcrumb";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { type Fact, FactsBox } from "@/components/ui/FactsBox";
import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { missingText } from "@/components/ui/Missing";
import { ErrorState, LoadingBlock, NotFound, PageHeader, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { StatusLabel } from "@/components/ui/StatusLabel";
import { t } from "@/i18n";
import {
  ApiError,
  type AttachedServer,
  type CreatedInstallKey,
  type Incident,
  type Me,
  type RelayCommand,
  type RelayDetail,
  type RevocationReason,
  type Server,
  api,
  describeError,
} from "@/lib/api";
import { updateAvailable } from "@/lib/agentVersion";
import { certificateState } from "@/lib/certificates";
import { formatDay, formatMinute, formatWhen } from "@/lib/clock";
import { formatOs, formatValue, unitOf } from "@/lib/format";
import { relayAddress } from "@/lib/install";
import { administersCustomer, administersEverything, multiCustomer } from "@/lib/me";
import { metricTitle } from "@/lib/metrics";
import { RELAYS_PATH, serverHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";
import { UUID } from "@/lib/uuid";

import { ConfirmRelayNames } from "./AddRelay";
import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";
import { RelayDependencies } from "./Dependencies";
import { OfflineAlerts } from "./OfflineAlerts";
import { SilenceBar } from "./Silences";
import { RemovedPanel, RemovePeer } from "./RemovePeer";
import { CertificateCell, relayStatus } from "./RelayList";
import { NewInstallKey } from "./TokensAdmin";

/** RFC 5280 reasons, as the revoke route takes them (ADR-0029). */
const REASONS: readonly RevocationReason[] = [
  "cessation_of_operation",
  "key_compromise",
  "affiliation_changed",
  "superseded",
  "unspecified",
];

/** A revocation reason in words; an unknown one as the server says it. */
function reasonWord(reason: string): string {
  return (REASONS as readonly string[]).includes(reason) ? t(`relays.reason.${reason as RevocationReason}`) : reason;
}

/** Open incidents fetched to fill the Problem column; the API has no per-relay filter. */
const INCIDENT_LIMIT = 500;

/** `/settings/relays/view/?id=…` (ADR-0020 routing). */
export function RelayView() {
  const id = useSearchParams().get("id");
  return <AdminLayout>{(me) => <RelayPage key={id} id={id} me={me} />}</AdminLayout>;
}

/** An attached server with what the servers and incidents lists add to it. */
interface Row extends AttachedServer {
  server?: Server;
  /** Its open incidents, worst first. */
  incidents: Incident[];
}

interface Data {
  relay: RelayDetail;
  rows: Row[];
  /** The newest release the server holds. */
  latest: string | null;
}

const severityRank = (incident: Incident) => (incident.severity === "critical" ? 0 : 1);

async function load(id: string, signal: AbortSignal): Promise<Data | null> {
  let relay: RelayDetail;
  try {
    relay = await api.relay(id, signal);
  } catch (e) {
    if (e instanceof ApiError && e.status === 404) return null;
    throw e;
  }
  const latest = await api.agentReleases(signal).then(
    (r) => r.latest,
    () => null,
  );
  if (relay.servers.length === 0) return { relay, rows: [], latest };
  const [{ servers }, { incidents }] = await Promise.all([
    api.servers(signal),
    api.incidents({ state: "open", limit: INCIDENT_LIMIT }, signal),
  ]);
  const rows = relay.servers.map((attached) => ({
    ...attached,
    server: servers.find((s) => s.id === attached.id),
    incidents: incidents.filter((i) => i.server_id === attached.id).sort((a, b) => severityRank(a) - severityRank(b)),
  }));
  return { relay, rows, latest };
}

const ACTIVE: RelayCommand["state"][] = ["queued", "sent", "running"];

/** The state of the relay's latest self-update, in a few words. */
export function updateText(update: RelayCommand): string {
  switch (update.state) {
    case "queued":
    case "sent":
      return t("relays.update.queued", { version: update.version });
    case "running":
      return t("relays.update.running", { version: update.version });
    case "succeeded":
      return t("relays.update.succeeded", { version: update.version });
    case "failed":
      return update.error
        ? t("relays.update.failedWith", { version: update.version, error: update.error })
        : t("relays.update.failed", { version: update.version });
    default:
      return t("relays.update.other", { version: update.version, state: update.state });
  }
}

/**
 * "Update relay to 0.3.0…", for administrators of the relay's customer
 * when the server holds a newer release than the relay runs (ADR-0055).
 */
function UpdateRelay({ relay, latest, me, onQueued }: { relay: RelayDetail; latest: string | null; me: Me; onQueued: () => void }) {
  const busy = relay.last_update && ACTIVE.includes(relay.last_update.state);
  if (
    relay.revoked_at ||
    busy ||
    !latest ||
    !updateAvailable(relay.relay_version ?? null, latest) ||
    !administersCustomer(me, relay.customer_id)
  ) {
    return null;
  }
  return (
    <InlineConfirm
      trigger={t("relays.update.trigger", { version: latest })}
      triggerVariant="default"
      title={t("relays.update.title", { name: relay.peer_id, version: latest })}
      consequence={t("relays.update.consequence", {
        version: latest,
        current: relay.relay_version ?? t("relays.update.current"),
      })}
      confirmLabel={t("relays.update.confirm", { name: relay.peer_id })}
      pendingLabel={t("relays.update.queuing")}
      onConfirm={async () => {
        await api.updateRelay(relay.id, latest);
        onQueued();
      }}
      describeError={(e) => t("relays.update.notQueued", { error: describeError(e) })}
    />
  );
}

/**
 * "Add servers behind this relay": a new install command of the relay's
 * site, already pointing at the relay's first name (ADR-0066).
 */
function AddServers({ relay, me }: { relay: RelayDetail; me: Me }) {
  const [created, setCreated] = useState<CreatedInstallKey | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function create() {
    setBusy(true);
    setError(null);
    try {
      setCreated(await api.createInstallKey({ site_id: relay.site_id, require_approval: false }));
    } catch (e) {
      setError(t("relays.add.createFailed", { error: describeError(e) }));
    } finally {
      setBusy(false);
    }
  }

  if (created) {
    return (
      <NewInstallKey
        installKey={created}
        editable={administersEverything(me)}
        relay={relayAddress(relay.server_names[0] ?? "")}
        onDone={() => setCreated(null)}
      />
    );
  }
  return (
    <div className={styles.buttons}>
      <Button variant="k" disabled={busy} onClick={() => void create()}>
        {t("relays.addServersBehind")}
      </Button>
      {error && (
        <p className={styles.formError} role="alert">
          {error}
        </p>
      )}
    </div>
  );
}

/** The worst open incident, else why an offline server is quiet. */
function Problem({ row, now }: { row: Row; now: Date }) {
  const [worst, ...others] = row.incidents;
  if (worst) {
    return (
      <>
        <StatusLabel kind="severity" value={worst.severity} />
        &nbsp; {metricTitle(worst.metric)}
        {worst.instance && ` ${worst.instance}`}{" "}
        <span className={styles.dim}>{formatValue(worst.last_value, unitOf(worst.metric))}</span>
        {others.length > 0 && <span className={styles.dim}> · {t("relays.more", { count: others.length })}</span>}
      </>
    );
  }
  if (row.status === "offline") {
    const last = row.server?.last_heartbeat;
    return (
      <span className={styles.dim}>
        {last ? t("relays.lastContact", { when: formatWhen(new Date(last), now) }) : t("relays.neverConnected")}
      </span>
    );
  }
  return null;
}

export function RelayPage({ id, me }: { id: string | null; me: Me }) {
  const valid = id !== null && UUID.test(id);
  const fetcher = useCallback((signal: AbortSignal) => load(id ?? "", signal), [id]);
  const { data, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher, { enabled: valid });

  const crumb = <Breadcrumb parents={[{ label: t("relays.title"), href: RELAYS_PATH }]} />;
  if (!valid || data === null) {
    return (
      <div className={styles.page}>
        {crumb}
        <NotFound
          title={t("relays.notFound.title")}
          actions={<ButtonLink href={RELAYS_PATH}>{t("relays.back")}</ButtonLink>}
        />
      </div>
    );
  }
  if (!data) {
    return (
      <div className={styles.page}>
        {crumb}
        {error ? (
          <ErrorState what={t("relays.whatOne")} error={describeError(error)} onRetry={refresh} />
        ) : (
          <LoadingBlock label={t("relays.loading")} />
        )}
      </div>
    );
  }

  const { relay, rows, latest } = data;
  const now = updatedAt ?? new Date();
  const { certificate } = relay;
  const facts: Fact[] = [
    { label: t("relays.facts.site"), value: multiCustomer(me) ? `${relay.site_name} · ${relay.customer_name}` : relay.site_name },
    {
      label: t("relays.facts.seen"),
      value: relay.last_heartbeat ? <RelativeTime value={relay.last_heartbeat} now={now} /> : t("relays.never"),
    },
    {
      label: t("relays.facts.since"),
      value: relay.status === "online" && relay.connected_at ? formatMinute(new Date(relay.connected_at)) : t("relays.facts.notConnected"),
    },
    {
      label: t("relays.facts.certificate"),
      value: certificate?.revoked ? (
        <StatusLabel kind="certificate" value="revoked" />
      ) : relay.cert_not_after && certificateState(relay.cert_not_after, now) !== "valid" ? (
        <>
          <CertificateCell notAfter={relay.cert_not_after} now={now} /> {formatDay(new Date(relay.cert_not_after))}
        </>
      ) : (
        <>
          <CertificateCell notAfter={relay.cert_not_after} now={now} />
          {relay.cert_not_after && t("relays.facts.renews")}
        </>
      ),
    },
    { label: t("relays.facts.version"), value: relay.relay_version ?? missingText("unknown") },
    ...(relay.last_update ? [{ label: t("relays.facts.selfUpdate"), value: updateText(relay.last_update) }] : []),
    {
      label: t("relays.facts.addresses"),
      value: relay.pending_names ? t("relays.waitingConfirmation") : relay.server_names.join(", ") || missingText("none"),
    },
    { label: t("relays.facts.added"), value: formatMinute(new Date(relay.created_at)) },
    ...(relay.revoked_at
      ? []
      : [{ label: t("relays.facts.offlineAlerts"), value: <OfflineAlerts kind="relays" target={relay} onChanged={refresh} /> }]),
    ...(relay.revoked_at
      ? [
          ...(certificate?.revocation_reason ? [
                {
                  label: t("relays.facts.reason"),
                  value: reasonWord(certificate.revocation_reason),
                },
              ] : []),
        ]
      : []),
  ];
  const details: Fact[] = [
    ...(certificate ? [{ label: t("relays.facts.serial"), value: certificate.serial, mono: true }] : []),
    ...(relay.cert_not_after ? [{ label: t("relays.facts.validUntil"), value: formatDay(new Date(relay.cert_not_after)) }] : []),
  ];

  const connected = rows.filter((row) => row.status === "online").length;
  const canAdminister = !relay.revoked_at && administersCustomer(me, relay.customer_id);
  const columns: Column<Row>[] = [
    {
      key: "host",
      header: t("relays.column.server"),
      cell: (row) => (
        <PrimaryCell
          title={row.hostname}
          href={serverHref(row.id)}
          sub={
            row.server &&
            [formatOs(row.server.os_family, row.server.os_version), row.server.agent_version && t("relays.agent", { version: row.server.agent_version })]
              .filter(Boolean)
              .join(" · ")
          }
        />
      ),
    },
    { key: "status", header: t("relays.column.status"), cell: (row) => <StatusLabel kind="server" value={row.status} /> },
    { key: "problem", header: t("relays.column.problem"), cell: (row) => <Problem row={row} now={now} /> },
    {
      key: "seen",
      header: t("relays.column.seen"),
      className: styles.dim,
      cell: (row) =>
        row.server?.last_heartbeat ? <RelativeTime value={row.server.last_heartbeat} now={now} /> : t("relays.never"),
    },
  ];

  return (
    <div className={styles.page}>
      {crumb}
      <PageHeader
        detail
        title={relay.peer_id}
        status={<StatusLabel kind="server" value={relayStatus(relay)} />}
        actions={
          !relay.revoked_at &&
          administersCustomer(me, relay.customer_id) && (
            <>
            <UpdateRelay relay={relay} latest={latest} me={me} onQueued={refresh} />
            <RemovePeer kind="relays" id={relay.id} name={relay.peer_id} onRemoved={refresh} />
            </>
          )
        }
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {relay.revoked_at && (
        <RemovedPanel
          kind="relay"
          name={relay.peer_id}
          osFamily="linux"
          removedAt={relay.revoked_at}
          canAdminister={administersCustomer(me, relay.customer_id)}
        />
      )}
      {relay.pending_names && canAdminister && <ConfirmRelayNames relay={relay} onConfirmed={refresh} />}
      {!relay.revoked_at && (
        <SilenceBar
          target={{ scope: "relay", id: relay.id, name: relay.peer_id, customerId: relay.customer_id, siteId: relay.site_id }}
        />
      )}
      <FactsBox columns={2} facts={facts} details={details} />
      <section className={styles.section} aria-labelledby="attached-title">
        <div className={styles.sectionHead}>
          <h2 id="attached-title" className={styles.sectionTitle}>
            {t("relays.attached.title", { name: relay.peer_id })}
          </h2>
          <span className={styles.dim}>{t("relays.attached.count", { count: rows.length, connected })}</span>
        </div>
        {canAdminister && !relay.pending_names && relay.server_names.length > 0 && (
          <AddServers relay={relay} me={me} />
        )}
        {rows.length === 0 ? (
          <p className={styles.sectionNote}>
            {relay.pending_names
              ? t("relays.attached.waiting")
              : t("relays.attached.none")}
          </p>
        ) : (
          <DataTable bare label={t("relays.attached.title", { name: relay.peer_id })} columns={columns} rows={rows} rowKey={(row) => row.id} />
        )}
      </section>
      {!relay.revoked_at && <RelayDependencies relay={relay} />}
    </div>
  );
}
