"use client";

import { type FormEvent, useCallback, useEffect, useId, useState } from "react";

import { t } from "@/i18n";
import {
  ApiError,
  type CloudCredentials,
  type ServerDetail,
  type SnmpCredentials,
  type VmwareCredentials,
  api,
  describeError,
} from "@/lib/api";
import { modifiesSite, multiCustomer, useMe } from "@/lib/me";
import { Button, fieldClass } from "@/components/ui/Button";
import { FactsBox } from "@/components/ui/FactsBox";
import { formatExact } from "@/components/ui/time";

import styles from "./AdminPages.module.css";
import { validAddress } from "./AddDevice";
import { EditCloud, cloudSummary } from "./CloudCredentials";
import { EditSnmp, snmpSummary } from "./SnmpCredentials";
import { EditVmware, vmwareSummary } from "./VmwareCredentials";

/** The Details of a device (ADR-0148): no agent, certificate or updates to speak of. */
export function DeviceFacts({ server, onChanged }: { server: ServerDetail; onChanged?: () => void }) {
  const me = useMe();
  // undefined while loading, null without credentials (ADR-0149).
  const [snmp, setSnmp] = useState<SnmpCredentials | null | undefined>(undefined);
  const [editing, setEditing] = useState(false);
  // The same for vCenter credentials (ADR-0156).
  const [vmware, setVmware] = useState<VmwareCredentials | null | undefined>(undefined);
  const [editingVmware, setEditingVmware] = useState(false);
  // And for a cloud account (ADR-0183).
  const [cloud, setCloud] = useState<CloudCredentials | null | undefined>(undefined);
  const [editingCloud, setEditingCloud] = useState(false);
  const canEdit = !server.revoked_at && modifiesSite(me, server.customer_id, server.site_id);
  const load = useCallback(
    (signal?: AbortSignal) =>
      api.deviceSnmp(server.id, signal).then(setSnmp, (e: unknown) => {
        if (e instanceof ApiError && e.status === 404) setSnmp(null);
      }),
    [server.id],
  );
  const loadVmware = useCallback(
    (signal?: AbortSignal) =>
      api.deviceVmware(server.id, signal).then(setVmware, (e: unknown) => {
        if (e instanceof ApiError && e.status === 404) setVmware(null);
      }),
    [server.id],
  );
  const loadCloud = useCallback(
    (signal?: AbortSignal) =>
      api.deviceCloud(server.id, signal).then(setCloud, (e: unknown) => {
        if (e instanceof ApiError && e.status === 404) setCloud(null);
      }),
    [server.id],
  );
  useEffect(() => {
    const controller = new AbortController();
    void load(controller.signal);
    void loadVmware(controller.signal);
    void loadCloud(controller.signal);
    return () => controller.abort();
  }, [load, loadVmware, loadCloud]);
  if (editingCloud && cloud !== undefined) {
    return (
      <EditCloud
        server={server}
        stored={cloud}
        onClose={() => setEditingCloud(false)}
        onSaved={() => {
          setEditingCloud(false);
          void loadCloud();
          onChanged?.();
        }}
      />
    );
  }
  if (editingVmware && vmware !== undefined) {
    return (
      <EditVmware
        server={server}
        stored={vmware}
        onClose={() => setEditingVmware(false)}
        onSaved={() => {
          setEditingVmware(false);
          void loadVmware();
          onChanged?.();
        }}
      />
    );
  }
  if (editing && snmp !== undefined) {
    return (
      <EditSnmp
        server={server}
        stored={snmp}
        onClose={() => setEditing(false)}
        onSaved={() => {
          setEditing(false);
          void load();
          onChanged?.();
        }}
      />
    );
  }
  return (
    <FactsBox
      columns={2}
      facts={[
        ...(multiCustomer(me) ? [{ label: t("server.fact.customer"), value: server.customer_name }] : []),
        { label: t("devices.fact.address"), value: server.address ?? "", mono: true },
        ...(snmp === undefined
          ? []
          : [
              {
                label: t("devices.snmp.fact"),
                value: (
                  <>
                    {snmpSummary(snmp)}{" "}
                    {canEdit && (
                      <Button variant="ghost" size="small" onClick={() => setEditing(true)}>
                        {snmp ? t("devices.snmp.change") : t("devices.snmp.set")}
                      </Button>
                    )}
                  </>
                ),
              },
            ]),
        // Shown once set, or to someone who can set it.
        ...(vmware === undefined || (vmware === null && !canEdit)
          ? []
          : [
              {
                label: t("virtualization.vmware.fact"),
                value: (
                  <>
                    {vmwareSummary(vmware)}{" "}
                    {canEdit && (
                      <Button variant="ghost" size="small" onClick={() => setEditingVmware(true)}>
                        {vmware ? t("virtualization.vmware.change") : t("virtualization.vmware.set")}
                      </Button>
                    )}
                  </>
                ),
              },
            ]),
        // Shown once set: a cloud account is added as one.
        ...(!cloud
          ? []
          : [
              {
                label: t("cloud.fact"),
                value: (
                  <>
                    {cloudSummary(cloud)}{" "}
                    {canEdit && (
                      <Button variant="ghost" size="small" onClick={() => setEditingCloud(true)}>
                        {t("cloud.change")}
                      </Button>
                    )}
                  </>
                ),
              },
            ]),
        { label: t("server.fact.added"), value: formatExact(new Date(server.created_at)).slice(0, 16) },
        ...(server.peer_id !== server.hostname ? [{ label: t("devices.fact.id"), value: server.peer_id, mono: true }] : []),
      ]}
    />
  );
}

/** Change a device's name or address. */
export function EditDevice({ server, onClose, onSaved }: { server: ServerDetail; onClose: () => void; onSaved: () => void }) {
  const titleId = useId();
  const [name, setName] = useState(server.hostname);
  const [address, setAddress] = useState(server.address ?? "");
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function submit(event: FormEvent) {
    event.preventDefault();
    if (name.trim() === "") return setProblem(t("devices.error.name"));
    if (!validAddress(address)) return setProblem(t("devices.error.address"));
    setBusy(true);
    setProblem(null);
    try {
      await api.updateDevice(server.id, { name: name.trim(), address: address.trim() });
      onSaved();
    } catch (e) {
      setProblem(t("devices.edit.failed", { error: describeError(e) }));
      setBusy(false);
    }
  }

  return (
    <form className={styles.panel} onSubmit={(e) => void submit(e)} aria-labelledby={titleId} noValidate>
      <h2 id={titleId} className={styles.sectionTitle}>
        {t("devices.edit.title", { name: server.hostname })}
      </h2>
      <label className={styles.field}>
        {t("devices.add.name")}
        <input className={fieldClass} value={name} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className={styles.field}>
        {t("devices.add.address")}
        <input
          className={fieldClass}
          value={address}
          spellCheck={false}
          autoCapitalize="none"
          onChange={(e) => setAddress(e.target.value)}
        />
      </label>
      <div className={styles.formFoot}>
        <Button variant="k" type="submit" disabled={busy}>
          {busy ? t("devices.edit.saving") : t("devices.edit.save")}
        </Button>
        <Button onClick={onClose} disabled={busy}>
          {t("devices.add.cancel")}
        </Button>
      </div>
      {problem && (
        <p className={styles.formError} role="alert">
          {problem}
        </p>
      )}
    </form>
  );
}
