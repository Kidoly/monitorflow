"use client";

import { type FormEvent, Fragment, type ReactNode, useCallback, useEffect, useState } from "react";

import {
  type GuestKind,
  type ProxmoxGuest,
  type ProxmoxSignal,
  type ProxmoxView,
  ApiError,
  api,
  describeError,
} from "@/lib/api";
import { snapshotReason } from "@/lib/remediation";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import styles from "./DetailPages.module.css";

/** "VM 101" or "container 104". */
export function guestName(kind: GuestKind, vmid: number): string {
  return `${kind === "lxc" ? "container" : "VM"} ${vmid}`;
}

/** "VM 104 on pve2", with its name when it says more: "VM 104 web-1 on pve2". */
function guestLabel(guest: { kind: GuestKind; vmid: number; node: string; name?: string | null }): string {
  const name = guest.name ? ` ${guest.name}` : "";
  return `${guestName(guest.kind, guest.vmid)}${name} on ${guest.node}`;
}

/** How discovery found the guest, ending a sentence. */
const SIGNALS: Record<ProxmoxSignal, string> = {
  uuid: "by its hardware ID",
  mac: "by its network card",
  hostname: "by its name only; check it is the right one",
};

/**
 * Whether AI's changes on the server are preceded by a Proxmox snapshot of
 * its VM or container, in words, with the mapping for administrators of
 * its customer (ADR-0044, ADR-0093). Nothing when Proxmox is off.
 */
export function ProxmoxRollback({ view, onChanged }: { view: ProxmoxView; onChanged: () => void }) {
  const [editing, setEditing] = useState(false);
  const { mapping, snapshots } = view;
  if (editing) {
    return (
      <MapForm
        view={view}
        onDone={() => {
          setEditing(false);
          onChanged();
        }}
        onCancel={() => setEditing(false)}
      />
    );
  }
  const change = (
    <Button size="small" onClick={() => setEditing(true)}>
      {mapping ? "Change" : (view.candidates ?? []).length > 0 ? "Another one" : "Map to a VM or container"}
    </Button>
  );
  const candidates = view.candidates ?? [];
  if (!mapping && candidates.length > 0) {
    return <Candidates view={view} candidates={candidates} change={change} onChanged={onChanged} />;
  }
  if (!mapping) {
    return (
      <>
        <Label tone="warn">Limited</Label>{" "}
        <span className={styles.muted}>
          Not mapped to a Proxmox VM or container, so AI&apos;s changes run without a snapshot.
        </span>{" "}
        {change}
      </>
    );
  }
  const guest = `${guestName(mapping.kind, mapping.vmid)} on ${mapping.node}`;
  if (!snapshots.available) {
    return (
      <>
        <Label tone="warn">Rollback limited</Label>{" "}
        {capitalize(snapshotReason(snapshots.reason, snapshots.kind ?? mapping.kind))}{" "}
        <span className={styles.muted}>
          {capitalize(guest)}, cluster {mapping.cluster}
          {snapshots.detail ? `: ${snapshots.detail}.` : "."}
        </span>{" "}
        {change}
      </>
    );
  }
  const matched = mapping.matched_by ?? null;
  const notThis = matched && <NotThisOne serverId={view.server_id} onChanged={onChanged} />;
  return (
    <>
      <Label tone="ok">Snapshot first</Label>{" "}
      {matched
        ? `Matched automatically to ${guestLabel(mapping)} ${SIGNALS[matched]}. Proxmox snapshots it before each AI change.`
        : `Proxmox snapshots ${guest} before each AI change.`}{" "}
      <span className={styles.muted}>
        {mapping.checked_at === null && "Proxmox is asked within a minute whether it can. "}
        {snapshots.left_out && `Snapshots leave out ${snapshots.left_out}: a rollback does not restore it. `}
        Cluster {mapping.cluster}.
      </span>{" "}
      {change} {notThis}
    </>
  );
}

/** "Not this one": unmaps an automatic match, which discovery then never makes again. */
function NotThisOne({ serverId, onChanged }: { serverId: string; onChanged: () => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <>
      <Button
        size="small"
        disabled={busy}
        onClick={() => {
          setBusy(true);
          setError(null);
          api.clearServerProxmox(serverId).then(onChanged, (e: unknown) => {
            setError(describeError(e));
            setBusy(false);
          });
        }}
      >
        Not this one
      </Button>
      {error && <span role="alert"> {error}</span>}
    </>
  );
}

/** Several guests match equally: one click picks the right one. */
function Candidates({
  view,
  candidates,
  change,
  onChanged,
}: {
  view: ProxmoxView;
  candidates: ProxmoxGuest[];
  change: ReactNode;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function pick(g: ProxmoxGuest) {
    setBusy(true);
    setError(null);
    try {
      await api.setServerProxmox(view.server_id, {
        cluster: g.cluster,
        node: g.node,
        vmid: g.vmid,
        kind: g.kind,
      });
      onChanged();
    } catch (e) {
      setError(describeError(e));
      setBusy(false);
    }
  }
  return (
    <>
      <Label tone="warn">Choose</Label> Several Proxmox VMs or containers match this server, so none is used yet. Which
      one is it?{" "}
      {candidates.map((g) => (
        <Fragment key={`${g.cluster}/${g.vmid}`}>
          <Button size="small" disabled={busy} onClick={() => void pick(g)}>
            {capitalize(guestLabel(g))}
          </Button>{" "}
        </Fragment>
      ))}{" "}
      {change}
      {error && <span role="alert"> {error}</span>}
    </>
  );
}

const capitalize = (text: string) => text.charAt(0).toUpperCase() + text.slice(1);

/** The server's Proxmox guest, loaded for administrators of its customer. */
export function useProxmox(serverId: string, enabled: boolean) {
  const fetcher = useCallback((signal: AbortSignal) => api.serverProxmox(serverId, signal), [serverId]);
  return usePolledData(fetcher, { enabled });
}

function MapForm({ view, onDone, onCancel }: { view: ProxmoxView; onDone: () => void; onCancel: () => void }) {
  const { mapping, clusters } = view;
  const [cluster, setCluster] = useState(mapping?.cluster ?? clusters[0] ?? "");
  const [kind, setKind] = useState<GuestKind>(mapping?.kind ?? "qemu");
  const [node, setNode] = useState(mapping?.node ?? "");
  const [vmid, setVmid] = useState(mapping ? String(mapping.vmid) : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Guests discovery saw, to pick from; by hand when there are none.
  const [guests, setGuests] = useState<ProxmoxGuest[] | null>(null);
  const [byHand, setByHand] = useState(false);
  useEffect(() => {
    const abort = new AbortController();
    api.serverProxmoxGuests(view.server_id, abort.signal).then(
      ({ guests }) => setGuests(guests.filter((g) => !g.taken)),
      () => setGuests([]),
    );
    return () => abort.abort();
  }, [view.server_id]);
  const picking = !byHand && guests !== null && guests.length > 0;
  const key = (g: { cluster: string; vmid: number }) => `${g.cluster}/${g.vmid}`;
  const [picked, setPicked] = useState(mapping ? key(mapping) : "");

  async function run(action: () => Promise<unknown>) {
    setBusy(true);
    setError(null);
    try {
      await action();
      onDone();
    } catch (e) {
      setError(
        e instanceof ApiError && e.status === 409
          ? "Another server is already mapped to this VM or container."
          : describeError(e),
      );
    } finally {
      setBusy(false);
    }
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    if (picking) {
      const g = guests.find((g) => key(g) === picked);
      if (!g) {
        setError("Choose the VM or container this server runs in.");
        return;
      }
      void run(() =>
        api.setServerProxmox(view.server_id, {
          cluster: g.cluster,
          node: g.node,
          vmid: g.vmid,
          kind: g.kind,
        }),
      );
      return;
    }
    const id = Number(vmid);
    if (!Number.isInteger(id) || id < 100) {
      setError("The ID is a number from 100, as Proxmox shows it.");
      return;
    }
    void run(() =>
      api.setServerProxmox(view.server_id, {
        cluster,
        node: node.trim(),
        vmid: id,
        kind,
      }),
    );
  }

  return (
    <form onSubmit={submit} aria-label="Proxmox VM or container" noValidate>
      {picking ? (
        <div className={admin.formGrid}>
          <label className={admin.field}>
            VM or container
            <select className={fieldClass} value={picked} onChange={(e) => setPicked(e.target.value)}>
              <option value="">Choose…</option>
              {guests.map((g) => (
                <option key={key(g)} value={key(g)}>
                  {capitalize(guestLabel(g))}
                  {clusters.length > 1 ? `, cluster ${g.cluster}` : ""}
                </option>
              ))}
            </select>
          </label>
        </div>
      ) : (
        <div className={admin.formGrid}>
          {clusters.length > 1 && (
            <label className={admin.field}>
              Cluster
              <select className={fieldClass} value={cluster} onChange={(e) => setCluster(e.target.value)}>
                {clusters.map((c) => (
                  <option key={c}>{c}</option>
                ))}
              </select>
            </label>
          )}
          <label className={admin.field}>
            Type
            <select className={fieldClass} value={kind} onChange={(e) => setKind(e.target.value as GuestKind)}>
              <option value="qemu">VM</option>
              <option value="lxc">Container (LXC)</option>
            </select>
          </label>
          <label className={admin.field}>
            Node
            <input className={fieldClass} value={node} onChange={(e) => setNode(e.target.value)} placeholder="pve1" />
          </label>
          <label className={admin.field}>
            ID
            <input
              className={fieldClass}
              inputMode="numeric"
              value={vmid}
              onChange={(e) => setVmid(e.target.value)}
              placeholder="104"
            />
          </label>
        </div>
      )}
      <div className={admin.formFoot}>
        <Button type="submit" variant="k" size="small" disabled={busy || !cluster}>
          Save
        </Button>
        {mapping && (
          <Button size="small" disabled={busy} onClick={() => void run(() => api.clearServerProxmox(view.server_id))}>
            Unmap
          </Button>
        )}
        <Button size="small" disabled={busy} onClick={onCancel}>
          Cancel
        </Button>
        {picking && (
          <Button size="small" disabled={busy} onClick={() => setByHand(true)}>
            Not in the list
          </Button>
        )}
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}
