"use client";

import { type FormEvent, useCallback, useState } from "react";

import { type GuestKind, type ProxmoxView, ApiError, api, describeError } from "@/lib/api";
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
      {mapping ? "Change" : "Map to a VM or container"}
    </Button>
  );
  if (!mapping) {
    return (
      <>
        <Label tone="warn">Limited</Label>{" "}
        <span className={styles.muted}>Not mapped to a Proxmox VM or container, so AI&apos;s changes run without a snapshot.</span>{" "}
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
  return (
    <>
      <Label tone="ok">Snapshot first</Label> Proxmox snapshots {guest} before each AI change.{" "}
      <span className={styles.muted}>
        {mapping.checked_at === null && "Proxmox is asked within a minute whether it can. "}
        {snapshots.left_out && `Snapshots leave out ${snapshots.left_out}: a rollback does not restore it. `}
        Cluster {mapping.cluster}.
      </span>{" "}
      {change}
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
    const id = Number(vmid);
    if (!Number.isInteger(id) || id < 100) {
      setError("The ID is a number from 100, as Proxmox shows it.");
      return;
    }
    void run(() => api.setServerProxmox(view.server_id, { cluster, node: node.trim(), vmid: id, kind }));
  }

  return (
    <form onSubmit={submit} aria-label="Proxmox VM or container" noValidate>
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
        {error && (
          <p className={admin.formError} role="alert">
            {error}
          </p>
        )}
      </div>
    </form>
  );
}
