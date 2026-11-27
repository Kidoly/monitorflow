"use client";

import type { IronError, UserInteraction } from "@devolutions/iron-remote-desktop";
import { useEffect, useRef } from "react";

import { t } from "@/i18n";
import { api, describeError } from "@/lib/api";
import { desktopSize, socketUrl } from "@/lib/remote";

import styles from "./RemoteDesktop.module.css";

/** Where a live desktop stands. */
export type DesktopState =
  | { step: "starting" }
  | { step: "connecting" }
  | { step: "open" }
  | { step: "closed"; reason: string };

/** The Windows account the person signs in with; kept only for the connection. */
export interface DesktopCredentials {
  username: string;
  password: string;
  domain: string;
}

/** What `ctrlAltDel` and closing need from a live desktop. */
export interface DesktopControls {
  ctrlAltDel: () => void;
}

/** IronRDP's error kinds (`IronErrorKind`), as numbers so the client loads only with the tab. */
const WRONG_PASSWORD = 1;
const LOGON_FAILURE = 2;
const ACCESS_DENIED = 3;
const RDCLEANPATH = 4;

function isIronError(value: unknown): value is IronError {
  return typeof value === "object" && value !== null && typeof (value as IronError).kind === "function";
}

/** Why the connection failed, in plain words. */
export function describeDesktopError(error: unknown): string {
  if (!isIronError(error)) return describeError(error);
  switch (error.kind()) {
    case WRONG_PASSWORD:
    case LOGON_FAILURE:
      return t("remote.desktop.wrongPassword");
    case ACCESS_DENIED:
      return t("remote.desktop.accessDenied");
    case RDCLEANPATH:
      return t("remote.desktop.refused");
    default:
      return t("remote.desktop.ended");
  }
}

/** IronRDP's web component and its RDP module, loaded only when a desktop is shown. */
async function loadClient(): Promise<{ element: HTMLElement; ready: Promise<UserInteraction> }> {
  const [, rdp] = await Promise.all([
    import("@devolutions/iron-remote-desktop"),
    import("@devolutions/iron-remote-desktop-rdp"),
  ]);
  await rdp.init("WARN");
  const element = document.createElement("iron-remote-desktop") as HTMLElement & { module?: unknown };
  element.module = rdp.Backend;
  element.setAttribute("scale", "fit");
  element.setAttribute("flexcenter", "true");
  element.setAttribute("verbose", "false");
  const ready = new Promise<UserInteraction>((resolve) => {
    element.addEventListener(
      "ready",
      (event) => resolve((event as CustomEvent<{ irgUserInteraction: UserInteraction }>).detail.irgUserInteraction),
      { once: true },
    );
  });
  return { element, ready };
}

/**
 * A live remote desktop of `serverId` (ADR-0190): starts a desktop session, then lets IronRDP's
 * web client speak RDP over the session's WebSocket, the central server doing the TLS with the
 * server's own Remote Desktop service through the agent. The password goes to Windows only.
 */
export function RemoteDesktop({
  serverId,
  hostname,
  credentials,
  onState,
  onControls,
}: {
  serverId: string;
  hostname: string;
  credentials: DesktopCredentials;
  /** Called as the session moves on, for the line above the desktop. */
  onState: (state: DesktopState, sessionId: string | null) => void;
  onControls: (controls: DesktopControls | null) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const report = useRef(onState);
  const controls = useRef(onControls);
  // Read once, when the session starts: a new sign-in is a new component.
  const account = useRef(credentials);
  useEffect(() => {
    report.current = onState;
    controls.current = onControls;
  }, [onState, onControls]);

  useEffect(() => {
    const container = box.current;
    if (!container) return;
    let cancelled = false;
    let closed = false;
    let ui: UserInteraction | null = null;
    let element: HTMLElement | null = null;
    let sessionId: string | null = null;
    const close = async (fallback: string) => {
      if (closed) return;
      closed = true;
      controls.current(null);
      // The central server knows best why it ended.
      let reason = fallback;
      if (sessionId) {
        try {
          reason = (await api.remoteSession(sessionId)).close_reason ?? fallback;
        } catch {
          // The fallback says enough.
        }
      }
      report.current({ step: "closed", reason }, sessionId);
    };
    void (async () => {
      let ready: Promise<UserInteraction>;
      try {
        const client = await loadClient();
        if (cancelled) return;
        element = client.element;
        ready = client.ready;
        container.appendChild(element);
        ui = await ready;
      } catch (e) {
        return close(t("remote.desktop.cannotLoad", { error: describeError(e) }));
      }
      if (cancelled) return;
      report.current({ step: "starting" }, null);
      let start;
      try {
        start = await api.startRemoteSession(serverId, "desktop");
      } catch (e) {
        return close(describeError(e));
      }
      if (cancelled) return;
      sessionId = start.session.id;
      report.current({ step: "connecting" }, sessionId);
      const { username, password, domain } = account.current;
      const config = ui
        .configBuilder()
        .withUsername(username)
        .withPassword(password)
        .withServerDomain(domain)
        .withDestination(hostname)
        .withProxyAddress(socketUrl(window.location, sessionId))
        .withAuthToken(start.token)
        .withDesktopSize(desktopSize(container.clientWidth))
        .build();
      // Only the client holds it now, for its network level authentication.
      account.current = { ...account.current, password: "" };
      try {
        const session = await ui.connect(config);
        if (cancelled) return;
        ui.setVisibility(true);
        const live = ui;
        controls.current({ ctrlAltDel: () => live.ctrlAltDel() });
        report.current({ step: "open" }, sessionId);
        const ended = await session.run();
        await close(ended.reason() || t("remote.desktop.ended"));
      } catch (e) {
        await close(describeDesktopError(e));
      }
    })();
    return () => {
      cancelled = true;
      closed = true;
      controls.current(null);
      try {
        ui?.shutdown();
      } catch {
        // Already ended.
      }
      if (sessionId) void api.closeRemoteSession(sessionId).catch(() => undefined);
      element?.remove();
    };
  }, [serverId, hostname]);

  return <div ref={box} className={styles.screen} aria-label={t("remote.desktop.label")} role="application" />;
}
