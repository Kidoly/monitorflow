"use client";

import dynamic from "next/dynamic";
import { useCallback, useState } from "react";

import { t } from "@/i18n";
import { type RemoteSession, type ServerDetail, api, describeError } from "@/lib/api";
import { WINDOWS_ALLOW_TERMINAL, allowTerminalCommand } from "@/lib/install";
import { administersSite, useMe } from "@/lib/me";
import { isOpen, terminalAcceptance } from "@/lib/remote";
import { type Polled, usePolledData } from "@/lib/usePolledData";
import { Button } from "@/components/ui/Button";
import { type Column, DataTable } from "@/components/ui/DataTable";
import { Banner, EmptyState, ErrorState, SkeletonLines } from "@/components/ui/PageState";
import { RelativeTime } from "@/components/ui/RelativeTime";
import { CopyButton } from "@/components/ui/SecretField";

import adminStyles from "./AdminPages.module.css";
import type { TerminalState } from "./RemoteTerminal";
import terminalStyles from "./RemoteTerminal.module.css";
import styles from "./ServerPage.module.css";

// xterm.js is loaded only on this tab.
const RemoteTerminal = dynamic(() => import("./RemoteTerminal").then((m) => m.RemoteTerminal), { ssr: false });
const RecordingView = dynamic(() => import("./RemoteTerminal").then((m) => m.RecordingView), { ssr: false });

/** "12 min", "1 h 5 min", "40 s". */
export function sessionLength(seconds: number | null): string {
  if (seconds === null) return t("remote.sessions.neverOpened");
  if (seconds < 60) return t("remote.sessions.seconds", { count: seconds });
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return t("remote.sessions.minutes", { count: minutes });
  return t("remote.sessions.hours", { hours: Math.floor(minutes / 60), minutes: minutes % 60 });
}

function statusLine(state: TerminalState | null): string {
  switch (state?.step) {
    case undefined:
    case "starting":
      return t("remote.terminal.starting");
    case "waiting":
      return t("remote.terminal.waiting");
    case "open":
      return t("remote.terminal.open");
    case "closed":
      return t("remote.terminal.closed", { reason: state.reason });
  }
}

/** The Terminal tab (ADR-0190): a live terminal, the server's past sessions and their recordings. */
export function ServerTerminalTab({ server }: { server: ServerDetail }) {
  const me = useMe();
  const access = server.remote_terminal;
  const accepts = terminalAcceptance(server);
  const admin = administersSite(me, server.customer_id, server.site_id);
  // A new key starts a new session; null shows none.
  const [run, setRun] = useState<number | null>(null);
  const [state, setState] = useState<TerminalState | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [replay, setReplay] = useState<string | null>(null);
  const fetcher = useCallback((signal: AbortSignal) => api.remoteSessions(server.id, signal), [server.id]);
  const sessions = usePolledData(fetcher, { enabled: access?.can_review ?? false });
  const refreshSessions = sessions.refresh;
  const onState = useCallback(
    (next: TerminalState, id: string | null) => {
      setState(next);
      if (id) setSessionId(id);
      if (next.step === "open" || next.step === "closed") refreshSessions();
    },
    [refreshSessions],
  );

  if (!access) return null;
  const canStart = access.can_open && accepts === "accepted" && !server.revoked_at;
  const live = run !== null && state?.step !== "closed";

  async function closeSession(id: string) {
    try {
      await api.closeRemoteSession(id);
    } finally {
      refreshSessions();
    }
  }

  return (
    <div className={styles.tab}>
      {access.can_open && accepts !== "accepted" && !server.revoked_at && (
        <NotAllowed server={server} accepts={accepts} admin={admin} />
      )}
      {canStart && (
        <div className={terminalStyles.recording}>
          <div className={terminalStyles.bar}>
            <span className={terminalStyles.status} role="status">
              {run === null ? t("remote.terminal.intro", { minutes: access.idle_timeout_minutes }) : statusLine(state)}
              {run === null && access.recorded && ` ${t("remote.terminal.recordedNote")}`}
            </span>
            {live ? (
              <Button
                size="small"
                onClick={() => {
                  if (sessionId) void closeSession(sessionId);
                  setRun(null);
                  setState(null);
                }}
              >
                {t("remote.terminal.close")}
              </Button>
            ) : (
              <Button
                variant="k"
                size="small"
                onClick={() => {
                  setState(null);
                  setSessionId(null);
                  setReplay(null);
                  setRun((n) => (n ?? 0) + 1);
                }}
              >
                {run === null ? t("remote.terminal.openButton") : t("remote.terminal.again")}
              </Button>
            )}
          </div>
          {run !== null && <RemoteTerminal key={run} serverId={server.id} onState={onState} />}
        </div>
      )}
      {replay && <RecordingView sessionId={replay} onClose={() => setReplay(null)} />}
      {access.can_review && (
        <RemoteSessionsTable
          kind="terminal"
          sessions={sessions}
          admin={admin}
          username={me?.username}
          onClose={closeSession}
          onReplay={setReplay}
        />
      )}
    </div>
  );
}

/**
 * The server's past sessions of one kind, newest first, with Close for open ones (who opened it, or an
 * administrator) and, for recorded ones, Replay for administrators.
 */
export function RemoteSessionsTable({
  kind,
  sessions,
  admin,
  username,
  onClose,
  onReplay,
}: {
  kind: RemoteSession["kind"];
  sessions: Polled<RemoteSession[]>;
  admin: boolean;
  username: string | undefined;
  onClose: (id: string) => Promise<void>;
  onReplay?: (id: string) => void;
}) {
  const columns: Column<RemoteSession>[] = [
    { key: "by", header: t("remote.sessions.by"), cell: (s) => s.requested_by },
    { key: "started", header: t("remote.sessions.started"), cell: (s) => <RelativeTime value={s.created_at} now={new Date()} /> },
    {
      key: "length",
      header: t("remote.sessions.length"),
      cell: (s) => (isOpen(s) && s.opened_at ? t("remote.sessions.openNow") : sessionLength(s.duration_seconds)),
    },
    {
      key: "end",
      header: t("remote.sessions.end"),
      cell: (s) => s.close_reason ?? t("remote.sessions.stillOpen"),
    },
    {
      key: "recorded",
      header: t("remote.sessions.recorded"),
      cell: (s: RemoteSession) => (s.recorded ? t("remote.sessions.yes") : t("remote.sessions.no")),
    },
  ];
  if (sessions.error && !sessions.data) {
    return <ErrorState what={t("remote.sessions.what")} error={describeError(sessions.error)} onRetry={sessions.refresh} />;
  }
  if (!sessions.data) return <SkeletonLines />;
  const rows = sessions.data.filter((s) => s.kind === kind);
  if (rows.length === 0) {
    return <EmptyState title={t("remote.sessions.none")}>{t("remote.sessions.noneNote")}</EmptyState>;
  }
  return (
    <DataTable
      label={kind === "terminal" ? t("remote.sessions.label") : t("remote.desktop.sessions")}
      columns={columns}
      rows={rows}
      rowKey={(s) => s.id}
      rowActions={(s) => (
        <>
          {onReplay && admin && s.recorded && s.opened_at && (
            <Button size="small" onClick={() => onReplay(s.id)}>
              {t("remote.sessions.replay")}
            </Button>
          )}
          {isOpen(s) && (admin || s.requested_by === username) && (
            <Button size="small" onClick={() => void onClose(s.id)}>
              {t("remote.sessions.close")}
            </Button>
          )}
        </>
      )}
    />
  );
}

/** Why no terminal opens here, with the line that allows it for administrators. */
function NotAllowed({
  server,
  accepts,
  admin,
}: {
  server: ServerDetail;
  accepts: ReturnType<typeof terminalAcceptance>;
  admin: boolean;
}) {
  if (accepts === "unknown") return <Banner tone="info">{t("remote.notYet")}</Banner>;
  if (accepts === "unsupported") return <Banner tone="info">{t("remote.unsupported")}</Banner>;
  return (
    <div className={styles.tab}>
      <Banner tone="info">{admin ? t("remote.notAllowedAdmin") : t("remote.notAllowed")}</Banner>
      {admin && <AllowTerminalLine osFamily={server.os_family} />}
    </div>
  );
}

function AllowTerminalLine({ osFamily }: { osFamily: string }) {
  const windows = osFamily === "windows";
  const fetcher = useCallback(
    (signal: AbortSignal) => Promise.all([api.agentInstall(signal), api.publicAddress(signal)]),
    [],
  );
  const { data } = usePolledData(fetcher, { enabled: !windows });
  const command = windows
    ? { text: WINDOWS_ALLOW_TERMINAL }
    : data && allowTerminalCommand(data[0], data[1], window.location.origin);
  if (!command) return null;
  if ("unavailable" in command) return <Banner tone="warn">{command.unavailable}</Banner>;
  return (
    <div className={adminStyles.snippet}>
      <pre className={adminStyles.code} aria-label={t("remote.allowLine")}>
        {command.text}
      </pre>
      <CopyButton text={command.text} name={t("remote.copy")} size="small" className={adminStyles.snippetCopy} />
    </div>
  );
}
