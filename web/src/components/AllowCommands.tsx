"use client";

import { useCallback } from "react";

import { api } from "@/lib/api";
import { WINDOWS_ALLOW_COMMANDS, allowCommandsCommand } from "@/lib/install";
import { usePolledData } from "@/lib/usePolledData";
import { Banner } from "@/components/ui/PageState";
import { CopyButton } from "@/components/ui/SecretField";

import styles from "./AdminPages.module.css";

/**
 * The line that lets a server run AI commands, ready to copy: the Linux installer
 * with its opt-in (ADR-0064), or the agent's own switch on Windows (ADR-0084).
 */
export function AllowCommandsLine({ osFamily = "linux" }: { osFamily?: string }) {
  return osFamily === "windows" ? <Snippet text={WINDOWS_ALLOW_COMMANDS} /> : <LinuxLine />;
}

function LinuxLine() {
  const fetcher = useCallback(
    (signal: AbortSignal) => Promise.all([api.agentInstall(signal), api.publicAddress(signal)]),
    [],
  );
  const { data } = usePolledData(fetcher);
  const command = data && allowCommandsCommand(data[0], data[1], window.location.origin);
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
