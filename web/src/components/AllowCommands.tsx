"use client";

import { useCallback } from "react";

import { api } from "@/lib/api";
import { WINDOWS_ALLOW_COMMANDS, WINDOWS_ALLOW_FROM_WEB, allowCommandsCommand } from "@/lib/install";
import { usePolledData } from "@/lib/usePolledData";
import { Banner } from "@/components/ui/PageState";
import { CopyButton } from "@/components/ui/SecretField";

import styles from "./AdminPages.module.css";

/**
 * The line that lets a server run AI commands, ready to copy: the Linux installer
 * with its opt-in (ADR-0064), or the agent's own switch on Windows (ADR-0084).
 * With `fromWeb`, the line that lets the web interface turn them on and off instead (ADR-0098).
 */
export function AllowCommandsLine({ osFamily = "linux", fromWeb = false }: { osFamily?: string; fromWeb?: boolean }) {
  if (osFamily === "windows") return <Snippet text={fromWeb ? WINDOWS_ALLOW_FROM_WEB : WINDOWS_ALLOW_COMMANDS} />;
  return <LinuxLine fromWeb={fromWeb} />;
}

function LinuxLine({ fromWeb }: { fromWeb: boolean }) {
  const fetcher = useCallback(
    (signal: AbortSignal) => Promise.all([api.agentInstall(signal), api.publicAddress(signal)]),
    [],
  );
  const { data } = usePolledData(fetcher);
  const command = data && allowCommandsCommand(data[0], data[1], window.location.origin, fromWeb);
  return (
    <>
      {command && "text" in command && <Snippet text={command.text} />}
      {command && "unavailable" in command && <Banner tone="warn">{command.unavailable}</Banner>}
    </>
  );
}

function Snippet({ text }: { text: string }) {
  return (
    <div className={styles.snippet}>
      <pre className={styles.code} aria-label="Command that allows AI commands">
        {text}
      </pre>
      <CopyButton text={text} name="Copy the command" size="small" className={styles.snippetCopy} />
    </div>
  );
}
