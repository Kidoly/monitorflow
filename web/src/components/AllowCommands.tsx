"use client";

import { useCallback } from "react";

import { api } from "@/lib/api";
import { allowCommandsCommand } from "@/lib/install";
import { usePolledData } from "@/lib/usePolledData";
import { Banner } from "@/components/ui/PageState";
import { CopyButton } from "@/components/ui/SecretField";

import styles from "./AdminPages.module.css";

/** The line that lets a Linux server run AI commands (ADR-0064), ready to copy. */
export function AllowCommandsLine() {
  const fetcher = useCallback(
    (signal: AbortSignal) => Promise.all([api.agentInstall(signal), api.publicAddress(signal)]),
    [],
  );
  const { data } = usePolledData(fetcher);
  const command = data && allowCommandsCommand(data[0], data[1], window.location.origin);
  return (
    <>
      {command && "text" in command && (
        <div className={styles.snippet}>
          <pre className={styles.code} aria-label="Command that allows AI commands">
            {command.text}
          </pre>
          <CopyButton text={command.text} name="Copy the command" size="small" className={styles.snippetCopy} />
        </div>
      )}
      {command && "unavailable" in command && <Banner tone="warn">{command.unavailable}</Banner>}
    </>
  );
}
