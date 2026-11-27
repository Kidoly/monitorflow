"use client";

import dynamic from "next/dynamic";
import { type FormEvent, useCallback, useState } from "react";

import { t } from "@/i18n";
import { type ServerDetail, api } from "@/lib/api";
import { WINDOWS_ALLOW_DESKTOP } from "@/lib/install";
import { administersSite, useMe } from "@/lib/me";
import { desktopAcceptance } from "@/lib/remote";
import { usePolledData } from "@/lib/usePolledData";
import { Button, fieldClass } from "@/components/ui/Button";
import { Banner } from "@/components/ui/PageState";
import { CopyButton } from "@/components/ui/SecretField";

import adminStyles from "./AdminPages.module.css";
import type { DesktopControls, DesktopCredentials, DesktopState } from "./RemoteDesktop";
import desktopStyles from "./RemoteDesktop.module.css";
import terminalStyles from "./RemoteTerminal.module.css";
import styles from "./ServerPage.module.css";
import { RemoteSessionsTable } from "./ServerTerminalTab";

// IronRDP's WebAssembly client, a few MB, is loaded only on this tab.
const RemoteDesktop = dynamic(() => import("./RemoteDesktop").then((m) => m.RemoteDesktop), { ssr: false });
const DesktopRecordingView = dynamic(() => import("./DesktopRecording").then((m) => m.DesktopRecordingView), {
  ssr: false,
});

function statusLine(state: DesktopState | null): string {
  switch (state?.step) {
    case undefined:
    case "starting":
      return t("remote.desktop.starting");
    case "connecting":
      return t("remote.desktop.connecting");
    case "open":
      return t("remote.desktop.open");
    case "closed":
      return t("remote.desktop.closed", { reason: state.reason });
  }
}

const EMPTY: DesktopCredentials = { username: "", password: "", domain: "" };

/**
 * The Remote desktop tab of a Windows server (ADR-0190): a sign-in with a Windows account, the live
 * desktop through the server's own Remote Desktop service, and the server's past desktop sessions with
 * the replay of recorded ones (ADR-0192).
 */
export function ServerDesktopTab({ server }: { server: ServerDetail }) {
  const me = useMe();
  const access = server.remote_terminal;
  const accepts = desktopAcceptance(server);
  const admin = administersSite(me, server.customer_id, server.site_id);
  const [form, setForm] = useState<DesktopCredentials>(EMPTY);
  // A new key starts a new session; null shows the sign-in.
  const [run, setRun] = useState<{ key: number; credentials: DesktopCredentials } | null>(null);
  const [state, setState] = useState<DesktopState | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [controls, setControls] = useState<DesktopControls | null>(null);
  const [replay, setReplay] = useState<string | null>(null);
  const fetcher = useCallback((signal: AbortSignal) => api.remoteSessions(server.id, signal), [server.id]);
  const sessions = usePolledData(fetcher, { enabled: access?.can_review ?? false });
  const refreshSessions = sessions.refresh;
  const onState = useCallback(
    (next: DesktopState, id: string | null) => {
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

  function connect(event: FormEvent) {
    event.preventDefault();
    setState(null);
    setSessionId(null);
    setRun((previous) => ({ key: (previous?.key ?? 0) + 1, credentials: form }));
    // The password stays with the client only, for the time it signs in.
    setForm({ ...form, password: "" });
  }

  return (
    <div className={styles.tab}>
      {access.can_open && accepts !== "accepted" && !server.revoked_at && <NotAllowed accepts={accepts} admin={admin} />}
      {canStart && (
        <div className={terminalStyles.recording}>
          <div className={terminalStyles.bar}>
            <span className={terminalStyles.status} role="status">
              {run === null ? t("remote.desktop.intro", { minutes: access.idle_timeout_minutes }) : statusLine(state)}
              {run === null && access.recorded && ` ${t("remote.terminal.recordedNote")}`}
            </span>
            {live && (
              <span className={adminStyles.buttonRow}>
                {controls && (
                  <Button size="small" onClick={controls.ctrlAltDel}>
                    {t("remote.desktop.ctrlAltDel")}
                  </Button>
                )}
                <Button
                  size="small"
                  onClick={() => {
                    if (sessionId) void closeSession(sessionId);
                    setRun(null);
                    setState(null);
                  }}
                >
                  {t("remote.desktop.close")}
                </Button>
              </span>
            )}
          </div>
          {!live && (
            <form className={desktopStyles.signIn} onSubmit={connect}>
              <label>
                {t("remote.desktop.user")}
                <input
                  className={fieldClass}
                  value={form.username}
                  onChange={(e) => setForm({ ...form, username: e.target.value })}
                  autoComplete="off"
                  spellCheck={false}
                  autoCapitalize="none"
                  required
                />
              </label>
              <label>
                {t("remote.desktop.password")}
                <input
                  className={fieldClass}
                  type="password"
                  value={form.password}
                  onChange={(e) => setForm({ ...form, password: e.target.value })}
                  // Never the saved MonitorFlow sign-in: it would go to Windows.
                  autoComplete="new-password"
                  required
                />
              </label>
              <label>
                {t("remote.desktop.domain")}
                <input
                  className={fieldClass}
                  value={form.domain}
                  onChange={(e) => setForm({ ...form, domain: e.target.value })}
                  placeholder={t("remote.desktop.domainHint")}
                  autoComplete="off"
                  spellCheck={false}
                  autoCapitalize="none"
                />
              </label>
              <Button variant="k" type="submit">
                {t("remote.desktop.connect")}
              </Button>
            </form>
          )}
          {run !== null && state?.step !== "closed" && (
            <RemoteDesktop
              key={run.key}
              serverId={server.id}
              hostname={server.hostname}
              credentials={run.credentials}
              onState={onState}
              onControls={setControls}
            />
          )}
        </div>
      )}
      {replay && <DesktopRecordingView sessionId={replay} onClose={() => setReplay(null)} />}
      {access.can_review && (
        <RemoteSessionsTable
          kind="desktop"
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

/** Why no desktop opens here, with the lines that allow it for administrators. */
function NotAllowed({ accepts, admin }: { accepts: ReturnType<typeof desktopAcceptance>; admin: boolean }) {
  if (accepts === "unknown") return <Banner tone="info">{t("remote.desktop.notYet")}</Banner>;
  if (accepts === "unsupported") return <Banner tone="info">{t("remote.desktop.unsupported")}</Banner>;
  return (
    <div className={styles.tab}>
      <Banner tone="info">{admin ? t("remote.desktop.notAllowedAdmin") : t("remote.desktop.notAllowed")}</Banner>
      {admin && (
        <div className={adminStyles.snippet}>
          <pre className={adminStyles.code} aria-label={t("remote.desktop.allowLine")}>
            {WINDOWS_ALLOW_DESKTOP}
          </pre>
          <CopyButton
            text={WINDOWS_ALLOW_DESKTOP}
            name={t("remote.copy")}
            size="small"
            className={adminStyles.snippetCopy}
          />
        </div>
      )}
    </div>
  );
}
