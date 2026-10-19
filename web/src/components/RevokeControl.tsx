"use client";

import { type FormEvent, useState } from "react";

import { type RevocationReason, type Revoked, api, describeError } from "@/lib/api";

import styles from "./Admin.module.css";

const REASONS: [RevocationReason, string][] = [
  ["unspecified", "Unspecified"],
  ["cessation_of_operation", "Decommissioned"],
  ["key_compromise", "Key compromised"],
  ["affiliation_changed", "Moved to another organisation"],
  ["superseded", "Replaced by a new enrollment"],
];

/**
 * A "Revoke" button that asks for confirmation and a reason, then revokes
 * the server or relay `id` (ADR-0029). Revocation cannot be undone.
 */
export function RevokeControl({
  kind,
  id,
  name,
  onRevoked,
}: {
  kind: "servers" | "relays";
  id: string;
  name: string;
  onRevoked: (result: Revoked) => void;
}) {
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState<RevocationReason>("unspecified");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const what = kind === "servers" ? "server" : "relay";

  async function submit(event: FormEvent) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const result = await api.revoke(kind, id, reason);
      setOpen(false);
      onRevoked(result);
    } catch (e) {
      setError(`Revocation failed. ${describeError(e)}`);
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <button className={styles.secondary} type="button" onClick={() => setOpen(true)}>
        Revoke {what}…
      </button>
    );
  }
  return (
    <form className={styles.confirm} onSubmit={submit} aria-label={`Revoke ${name}`}>
      <p>
        <strong>Revoke {name}?</strong> Its certificates go into the revocation list, its connections close
        now{kind === "relays" && ", including the agents it forwards"}, and it cannot connect again. This
        cannot be undone: enrolling it again needs a new ID.
      </p>
      <div className={styles.form}>
        <label>
          Reason
          <select value={reason} onChange={(e) => setReason(e.target.value as RevocationReason)}>
            {REASONS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </select>
        </label>
        <button className={styles.danger} type="submit" disabled={busy}>
          Revoke {name}
        </button>
        <button className={styles.secondary} type="button" onClick={() => setOpen(false)} disabled={busy}>
          Cancel
        </button>
      </div>
      {error && (
        <p className={styles.error} role="alert">
          {error}
        </p>
      )}
    </form>
  );
}
