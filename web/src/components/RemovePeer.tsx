"use client";

import Link from "next/link";
import { useEffect, useId, useState } from "react";

import { rich, t } from "@/i18n";
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
      trigger={t("server.remove.trigger", { name })}
      title={t("server.remove.title", { name })}
      consequence={kind === "relays" ? t("server.remove.consequenceRelay") : t("server.remove.consequence")}
      confirmLabel={t("server.remove.confirm", { name })}
      pendingLabel={t("server.remove.removing")}
      onConfirm={async () => {
        await api.revoke(kind, id, "cessation_of_operation");
        onRemoved();
      }}
      describeError={(e) => t("server.remove.failed", { error: describeError(e) })}
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
    <section className={styles.confirm} aria-label={t("server.remove.label")}>
      <p>
        <strong>{t("server.remove.removed", { name })}</strong>
        {t("server.remove.removedAt", { at: formatExact(new Date(removedAt)).slice(0, 16) })}
      </p>
      {canAdminister && (
        <>
          <p id={labelId}>
            {kind === "relay"
              ? t(osFamily === "windows" ? "server.remove.runRelayWindows" : "server.remove.runRelay", { name })
              : t(osFamily === "windows" ? "server.remove.runAgentWindows" : "server.remove.runAgent", { name })}
          </p>
          {command && (
            <div className={pageStyles.snippet}>
              <pre className={pageStyles.code} aria-labelledby={labelId}>
                {command}
              </pre>
              <CopyButton text={command} name={t("server.remove.copy")} size="small" className={pageStyles.snippetCopy} />
            </div>
          )}
          {error && (
            <p className={styles.error} role="alert">
              {t("server.remove.loadFailed", { error })}
            </p>
          )}
          <p>
            {kind === "relay"
              ? rich("server.remove.addBackRelay", {
                  link: <Link href={TOKENS_PATH}>{t("server.remove.addServers")}</Link>,
                  name,
                })
              : rich("server.remove.addBack", { link: <Link href={TOKENS_PATH}>{t("server.remove.addServers")}</Link> })}
          </p>
        </>
      )}
    </section>
  );
}
