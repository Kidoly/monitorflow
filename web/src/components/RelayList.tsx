"use client";

import Link from "next/link";
import { useCallback, useState } from "react";

import { Button } from "@/components/ui/Button";
import { type Column, DataTable, PrimaryCell } from "@/components/ui/DataTable";
import { Banner, EmptyState, ErrorState, PageHeader, RefreshLine, StaleBanner } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { Label, StatusLabel } from "@/components/ui/StatusLabel";
import { type Me, type Relay, api, describeError } from "@/lib/api";
import { certificateState } from "@/lib/certificates";
import { daysUntil, formatDay } from "@/lib/clock";
import { administersCustomer } from "@/lib/me";
import { relayHref } from "@/lib/routes";
import { usePolledData } from "@/lib/usePolledData";

import { AddRelay, ConfirmRelayNames } from "./AddRelay";
import { AdminLayout } from "./AdminLayout";
import styles from "./AdminPages.module.css";

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
  const [adding, setAdding] = useState(false);
  const [confirmed, setConfirmed] = useState<Relay | null>(null);
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
        relay.pending_names ? (
          <Label tone="warn">Waiting for confirmation</Label>
        ) : relay.server_names.length > 0 ? (
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
          canEnroll &&
          !adding && (
            <Button variant="k" onClick={() => setAdding(true)}>
              Add relay
            </Button>
          )
        }
      />
      {stale && updatedAt && (
        <StaleBanner error={describeError(error)} updatedAt={updatedAt} failures={failures} onRetry={refresh} />
      )}
      {adding && <AddRelay me={me} onDone={() => setAdding(false)} />}
      {relays
        ?.filter((relay) => relay.pending_names && administersCustomer(me, relay.customer_id))
        .map((relay) => (
          <ConfirmRelayNames
            key={relay.id}
            relay={relay}
            onConfirmed={() => {
              setConfirmed(relay);
              refresh();
            }}
          />
        ))}
      {confirmed && (
        <Banner tone="info">
          {confirmed.peer_id} is ready.{" "}
          <Link href={relayHref(confirmed.id)}>Add servers behind this relay</Link>
        </Banner>
      )}
      {!relays && error ? (
        <ErrorState what="relays" error={describeError(error)} onRetry={refresh} />
      ) : relays?.length === 0 ? (
        !adding && (
          <EmptyState
            title="No relays yet"
            actions={
              canEnroll && (
                <Button variant="k" onClick={() => setAdding(true)}>
                  Add relay
                </Button>
              )
            }
          >
            A relay carries the servers of a site that cannot reach this server directly. It takes one command on one
            of the site&apos;s hosts.
          </EmptyState>
        )
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
