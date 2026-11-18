"use client";

import { type FormEvent, useId, useState } from "react";

import { t } from "@/i18n";
import { type ServerDetail, api, describeError } from "@/lib/api";
import { multiCustomer, useMe } from "@/lib/me";
import { Button, fieldClass } from "@/components/ui/Button";
import { FactsBox } from "@/components/ui/FactsBox";
import { formatExact } from "@/components/ui/time";

import styles from "./AdminPages.module.css";
import { validAddress } from "./AddDevice";

/** The Details of a device (ADR-0148): no agent, certificate or updates to speak of. */
export function DeviceFacts({ server }: { server: ServerDetail }) {
  const me = useMe();
  return (
    <FactsBox
      columns={2}
      facts={[
        ...(multiCustomer(me) ? [{ label: t("server.fact.customer"), value: server.customer_name }] : []),
        { label: t("devices.fact.address"), value: server.address ?? "", mono: true },
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
