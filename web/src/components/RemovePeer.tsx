"use client";

import Link from "next/link";
import { useEffect, useId, useState } from "react";

import { InlineConfirm } from "@/components/ui/InlineConfirm";
import { CopyButton } from "@/components/ui/SecretField";
import { formatExact } from "@/components/ui/time";
import { api, describeError } from "@/lib/api";
import { uninstallCommand } from "@/lib/install";
import { TOKENS_PATH } from "@/lib/routes";

import styles from "./Admin.module.css";
import pageStyles from "./AdminPages.module.css";

/**
 * "Remove web-01…": revokes the server or relay, hides it from lists and
 * counts and closes its incidents (ADR-0062). For administrators of its
 * customer. The page then shows {@link RemovedPanel}.
 */
export function RemovePeer({
  kind,
  id,
  name,
  onRemoved,
}: {
  kind: "servers" | "relays";
  id: string;
  name: string;
  onRemoved: () => void;
}) {
  return (
    <InlineConfirm
      trigger={`Remove ${name}…`}
      title={`Remove ${name}?`}
      consequence={
        `It stops connecting${kind === "relays" ? ", with the agents it forwards," : ""} and leaves every list. ` +
        "Its open incidents close and its history is kept. You can add it back later with the install command. " +
        "The next screen shows how to uninstall it from the host."
      }
      confirmLabel={`Remove ${name}`}
      pendingLabel="Removing…"
      onConfirm={async () => {
        await api.revoke(kind, id, "cessation_of_operation");
        onRemoved();
      }}
      describeError={(e) => `Not removed. ${describeError(e)}`}
    />
  );
}

/** What is left to do on the host of a removed server or relay. */
export function RemovedPanel({
  kind,
  name,
  osFamily,
  removedAt,
  canAdminister,
}: {
  kind: "agent" | "relay";
  name: string;
  osFamily: string;
  removedAt: string;
  canAdminister: boolean;
}) {
  const [command, setCommand] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const labelId = useId();

  useEffect(() => {
    if (!canAdminister) return;
    const controller = new AbortController();
    const { signal } = controller;
    Promise.all([api.agentInstall(signal), api.publicAddress(signal)]).then(
      ([install, address]) => setCommand(uninstallCommand(kind, osFamily, install, address, window.location.origin)),
      (e) => {
        if (!signal.aborted) setError(describeError(e));
      },
    );
    return () => controller.abort();
  }, [canAdminister, kind, osFamily]);

  return (
    <section className={styles.confirm} aria-label="Removed">
      <p>
        <strong>{name} was removed</strong> on {formatExact(new Date(removedAt)).slice(0, 16)}. It no longer connects, and
        its history stays here.
      </p>
      {canAdminister && (
        <>
          <p id={labelId}>
            Run this on {name} {osFamily === "windows" ? "in an elevated PowerShell" : "as a user with sudo"} to uninstall
            the {kind}:
          </p>
          {command && (
            <div className={pageStyles.snippet}>
              <pre className={pageStyles.code} aria-labelledby={labelId}>
                {command}
              </pre>
              <CopyButton text={command} name="Copy the uninstall command" size="small" className={pageStyles.snippetCopy} />
            </div>
          )}
          {error && (
            <p className={styles.error} role="alert">
              The command could not be loaded. {error}
            </p>
          )}
          <p>
            To add it back, uninstall it first, then run the install command from{" "}
            <Link href={TOKENS_PATH}>{kind === "relay" ? "Enrollment tokens" : "Add servers"}</Link> on the host
            {kind === "relay" && ` with the ID ${name}`}. It returns in the same site, with its history.
          </p>
        </>
      )}
    </section>
  );
}
