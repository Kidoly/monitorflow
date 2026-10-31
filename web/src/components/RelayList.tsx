"use client";

import { useCallback } from "react";

import { ButtonLink } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { type Me, type Relay, api, describeError } from "@/lib/api";
import { certificateState } from "@/lib/certificates";
import { daysUntil, formatDay } from "@/lib/clock";
import { administersCustomer } from "@/lib/me";
import { TOKENS_PATH, relayHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

/** Where "New relay token" leads: the token form with Relay chosen. */
export const NEW_RELAY_TOKEN_HREF = `${TOKENS_PATH}?role=relay`;

/** `/admin/relays/`: every relay the user reads. */
export function RelayListPage() {
  return <AdminLayout>{(me) => <RelayList me={me} />}</AdminLayout>;
}

/** Online, Offline, or Revoked once revoked whatever the stream says. */
export function relayStatus(relay: Pick<Relay, "status" | "revoked_at">) {
  return relay.revoked_at ? "revoked" : relay.status;
}

/**
 * "Valid · 2027-02-11", or a tinted "Expires in 5 days" / "Expired" under
 * `CERT_WARNING_DAYS`; "None" without an unrevoked certificate.
 */
export function CertificateCell({ notAfter, now }: { notAfter: string | null; now: Date }) {
  if (!notAfter) return <span className={styles.dim}>None</span>;
  const date = new Date(notAfter);
  switch (certificateState(notAfter, now)) {
    case "valid":
      return <>Valid · {formatDay(date)}</>;
    case "expiring": {
      const days = daysUntil(date, now);
      return <Label tone="warn">{days <= 1 ? "Expires in 1 day" : `Expires in ${days} days`}</Label>;
    }
    case "expired":
      return <Label tone="bad">Expired</Label>;
  }
}

const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;

export function RelayList({ me }: { me: Me }) {
  const fetcher = useCallback(async (signal: AbortSignal) => (await api.relays(signal)).relays, []);
  const { data: relays, error, stale, failures, updatedAt, refresh } = usePolledData(fetcher);
  const now = updatedAt ?? new Date();

  const columns: Column<Relay>[] = [
    {
      key: "relay",
      header: "Relay",
      cell: (relay) => (
        <PrimaryCell
          title={relay.peer_id}
          href={relayHref(relay.id)}
          sub={[relay.site_name, relay.customer_name, relay.relay_version && `relay ${relay.relay_version}`]
            .filter(Boolean)
            .join(" · ")}
        />
      ),
    },
    { key: "status", header: "Status", cell: (relay) => <StatusLabel kind="server" value={relayStatus(relay)} /> },
    {
      key: "agents",
      header: "Agents",
      cell: (relay) => (
        <>
          <strong>{relay.connected_agents}</strong>{" "}
          <span className={styles.dim}>connected / {relay.server_count} total</span>
        </>
      ),
    },
    {
      key: "names",
      header: "Server names",
      cell: (relay) =>
        relay.server_names.length > 0 ? (
          <span className={styles.mono}>{relay.server_names.join(" · ")}</span>
        ) : (
          <span className={styles.dim}>—</span>
        ),
    },
    {
      key: "certificate",
      header: "Certificate",
      cell: (relay) => <CertificateCell notAfter={relay.cert_not_after} now={now} />,
    },
    {
      key: "seen",
      header: "Last seen",
      className: styles.dim,
      cell: (relay) => (relay.last_heartbeat ? <RelativeTime value={relay.last_heartbeat} now={now} /> : "never"),
    },
  ];

  const online = relays?.filter((relay) => relayStatus(relay) === "online").length ?? 0;
  const canEnroll = administersCustomer(me);
  return (
    <div className={styles.page}>
      <PageHeader
        title="Relays"
        meta={
          relays || error ? (
            <>
              {relays && `${plural(relays.length, "relay")}, ${online} online · `}
              <RefreshLine updatedAt={updatedAt} stale={stale} />
            </>
          ) : undefined
        }
        actions={
          canEnroll && (
            <ButtonLink variant="k" href={NEW_RELAY_TOKEN_HREF}>
              New relay token
            </ButtonLink>
          )
        }
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {!relays && error ? (
        <ErrorState what="relays" error={describeError(error)} onRetry={refresh} />
      ) : relays?.length === 0 ? (
        <EmptyState
          title="No relays yet"
          actions={
            canEnroll && (
              <ButtonLink variant="k" href={NEW_RELAY_TOKEN_HREF}>
                New relay token
              </ButtonLink>
            )
          }
        >
          A relay enrolls with a relay enrollment token, then agents of its site connect through it.
        </EmptyState>
      ) : (
        <DataTable
          label="Relays"
          columns={columns}
          rows={relays ?? []}
          rowKey={(relay) => relay.id}
          loading={!relays}
        />
      )}
    </div>
  );
}
