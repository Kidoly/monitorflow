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
import { t } from "@/i18n";
import { snapshotReason } from "@/lib/remediation";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { Label } from "@/components/ui/StatusLabel";

import admin from "./AdminPages.module.css";
import styles from "./DetailPages.module.css";

/** "VM 101" or "container 104". */
export function guestName(kind: GuestKind, vmid: number): string {
  return t(kind === "lxc" ? "rollback.container" : "rollback.vm", { id: String(vmid) });
}

/** "VM 104 on pve2", with its name when it says more: "VM 104 web-1 on pve2". */
function guestLabel(guest: { kind: GuestKind; vmid: number; node: string; name?: string | null }): string {
  const name = guestName(guest.kind, guest.vmid);
  return guest.name
    ? t("rollback.namedGuestOn", { guest: name, name: guest.name, node: guest.node })
    : t("rollback.guestOn", { guest: name, node: guest.node });
}

/** How discovery found the guest, ending a sentence. */
const signalText = (signal: ProxmoxSignal) => t(`rollback.signal.${signal}`);

/**
 * Whether AI's changes on the server are preceded by a Proxmox snapshot of
 * its VM or container, in words, with the mapping for administrators of
 * its customer (ADR-0044, ADR-0093). Nothing when Proxmox is off. A server
 * that is no guest shows its `local` line instead, with the mapping button.
 */
export function ProxmoxRollback({
  view,
  onChanged,
  local,
}: {
  view: ProxmoxView;
  onChanged: () => void;
  /** The local snapshot line, for a server that is no Proxmox guest (ADR-0094). */
  local?: ReactNode;
}) {
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
      {mapping ? t("rollback.change") : (view.candidates ?? []).length > 0 ? t("rollback.another") : t("rollback.map")}
    </Button>
  );
  const candidates = view.candidates ?? [];
  if (!mapping && candidates.length > 0) {
    return <Candidates view={view} candidates={candidates} change={change} onChanged={onChanged} />;
  }
  if (!mapping && local) {
    return (
      <>
        {local} {change}
      </>
    );
  }
  if (!mapping) {
    return (
      <>
        <Label tone="warn">{t("rollback.limited")}</Label> <span className={styles.muted}>{t("rollback.notMapped")}</span>{" "}
        {change}
      </>
    );
  }
  const guest = t("rollback.guestOn", { guest: guestName(mapping.kind, mapping.vmid), node: mapping.node });
  if (!snapshots.available) {
    return (
      <>
        <Label tone="warn">{t("rollback.rollbackLimited")}</Label>{" "}
        {capitalize(snapshotReason(snapshots.reason, snapshots.kind ?? mapping.kind))}{" "}
        <span className={styles.muted}>
          {snapshots.detail
            ? t("rollback.clusterDetail", { guest: capitalize(guest), cluster: mapping.cluster, detail: snapshots.detail })
            : t("rollback.cluster", { guest: capitalize(guest), cluster: mapping.cluster })}
        </span>{" "}
        {change}
      </>
    );
  }
  const matched = mapping.matched_by ?? null;
  const notThis = matched && <NotThisOne serverId={view.server_id} onChanged={onChanged} />;
  return (
    <>
      <Label tone="ok">{t("rollback.snapshotFirst")}</Label>{" "}
      {matched
        ? t("rollback.matched", { guest: guestLabel(mapping), signal: signalText(matched) })
        : t("rollback.snapshots", { guest })}{" "}
      <span className={styles.muted}>
        {mapping.checked_at === null && `${t("rollback.asking")} `}
        {snapshots.left_out && `${t("rollback.leftOut", { left: snapshots.left_out })} `}
        {t("rollback.clusterName", { cluster: mapping.cluster })}
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
        {t("rollback.notThisOne")}
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
      <Label tone="warn">{t("rollback.choose")}</Label> {t("rollback.several")}{" "}
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
          ? t("rollback.taken")
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
        setError(t("rollback.pick"));
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
      setError(t("rollback.badId"));
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
    <form onSubmit={submit} aria-label={t("rollback.form")} noValidate>
      {picking ? (
        <div className={admin.formGrid}>
          <label className={admin.field}>
            {t("rollback.guest")}
            <select className={fieldClass} value={picked} onChange={(e) => setPicked(e.target.value)}>
              <option value="">{t("rollback.chooseOption")}</option>
              {guests.map((g) => (
                <option key={key(g)} value={key(g)}>
                  {capitalize(guestLabel(g))}
                  {clusters.length > 1 ? t("rollback.inCluster", { cluster: g.cluster }) : ""}
                </option>
              ))}
            </select>
          </label>
        </div>
      ) : (
        <div className={admin.formGrid}>
          {clusters.length > 1 && (
            <label className={admin.field}>
              {t("rollback.clusterLabel")}
              <select className={fieldClass} value={cluster} onChange={(e) => setCluster(e.target.value)}>
                {clusters.map((c) => (
                  <option key={c}>{c}</option>
                ))}
              </select>
            </label>
          )}
          <label className={admin.field}>
            {t("rollback.type")}
            <select className={fieldClass} value={kind} onChange={(e) => setKind(e.target.value as GuestKind)}>
              <option value="qemu">{t("rollback.vmOption")}</option>
              <option value="lxc">{t("rollback.containerOption")}</option>
            </select>
          </label>
          <label className={admin.field}>
            {t("rollback.node")}
            <input className={fieldClass} value={node} onChange={(e) => setNode(e.target.value)} placeholder="pve1" />
          </label>
          <label className={admin.field}>
            {t("rollback.id")}
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
          {t("rollback.save")}
        </Button>
        {mapping && (
          <Button size="small" disabled={busy} onClick={() => void run(() => api.clearServerProxmox(view.server_id))}>
            {t("rollback.unmap")}
          </Button>
        )}
        <Button size="small" disabled={busy} onClick={onCancel}>
          {t("rollback.cancel")}
        </Button>
        {picking && (
          <Button size="small" disabled={busy} onClick={() => setByHand(true)}>
            {t("rollback.notListed")}
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
