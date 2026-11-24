"use client";

import "@xterm/xterm/css/xterm.css";

import type { Terminal as XTerm } from "@xterm/xterm";
import { useEffect, useRef, useState } from "react";

import { t } from "@/i18n";
import { type RemoteRecording, api, describeError } from "@/lib/api";
import { decodeChunk, parseEvent, socketUrl } from "@/lib/remote";
import { Button } from "@/components/ui/Button";

import styles from "./RemoteTerminal.module.css";

/** Where a live session stands. */
export type TerminalState =
  | { step: "starting" }
  | { step: "waiting" }
  | { step: "open" }
  | { step: "closed"; reason: string };

/** A dark terminal in both themes, with the scrollbar hidden: the wheel still scrolls. */
const THEME = {
  background: "#0c0c0e",
  foreground: "#ededef",
  cursor: "#ededef",
  selectionBackground: "#3a3a41",
  scrollbarSliderBackground: "transparent",
  scrollbarSliderHoverBackground: "transparent",
  scrollbarSliderActiveBackground: "transparent",
};

/** xterm.js and its fit addon, loaded only when a terminal is shown. */
async function loadTerminal(element: HTMLElement, readOnly: boolean) {
  const [{ Terminal }, { FitAddon }] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]);
  const terminal = new Terminal({
    cursorBlink: !readOnly,
    disableStdin: readOnly,
    fontFamily: 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace',
    fontSize: 13,
    scrollback: 5000,
    theme: THEME,
  });
  const fit = new FitAddon();
  terminal.loadAddon(fit);
  terminal.open(element);
  fit.fit();
  const observer = new ResizeObserver(() => fit.fit());
  observer.observe(element);
  return {
    terminal,
    dispose: () => {
      observer.disconnect();
      terminal.dispose();
    },
  };
}

/**
 * A live terminal on `serverId` (ADR-0190): starts a session, opens its WebSocket with the
 * one-time token, and carries keystrokes, output and size changes until the session ends.
 */
export function RemoteTerminal({
  serverId,
  onState,
}: {
  serverId: string;
  /** Called as the session moves on, for the line above the terminal. */
  onState: (state: TerminalState, sessionId: string | null) => void;
}) {
  const box = useRef<HTMLDivElement>(null);
  const report = useRef(onState);
  useEffect(() => {
    report.current = onState;
  }, [onState]);

  useEffect(() => {
    const element = box.current;
    if (!element) return;
    let cancelled = false;
    let socket: WebSocket | null = null;
    let dispose: (() => void) | null = null;
    let closed = false;
    const close = (reason: string, sessionId: string | null) => {
      if (closed) return;
      closed = true;
      report.current({ step: "closed", reason }, sessionId);
    };
    void (async () => {
      let term: XTerm;
      try {
        const loaded = await loadTerminal(element, false);
        if (cancelled) return loaded.dispose();
        dispose = loaded.dispose;
        term = loaded.terminal;
      } catch (e) {
        return close(t("remote.terminal.cannotLoad", { error: describeError(e) }), null);
      }
      report.current({ step: "starting" }, null);
      let start;
      try {
        start = await api.startRemoteSession(serverId);
      } catch (e) {
        return close(describeError(e), null);
      }
      if (cancelled) return;
      const id = start.session.id;
      const ws = new WebSocket(socketUrl(window.location, id));
      socket = ws;
      ws.binaryType = "arraybuffer";
      const encoder = new TextEncoder();
      ws.onopen = () => ws.send(JSON.stringify({ token: start.token, cols: term.cols, rows: term.rows }));
      ws.onmessage = (message: MessageEvent<ArrayBuffer | string>) => {
        if (typeof message.data !== "string") {
          term.write(new Uint8Array(message.data));
          return;
        }
        const event = parseEvent(message.data);
        if (event?.type === "waiting") report.current({ step: "waiting" }, id);
        if (event?.type === "opened") {
          report.current({ step: "open" }, id);
          term.focus();
        }
        if (event?.type === "closed") close(event.reason || t("remote.terminal.ended"), id);
      };
      ws.onclose = () => close(t("remote.terminal.ended"), id);
      term.onData((data) => {
        if (ws.readyState === WebSocket.OPEN) ws.send(encoder.encode(data));
      });
      term.onResize(({ cols, rows }) => {
        if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ type: "resize", cols, rows }));
      });
    })();
    return () => {
      cancelled = true;
      if (socket && socket.readyState === WebSocket.OPEN) socket.send(JSON.stringify({ type: "close" }));
      socket?.close();
      dispose?.();
    };
  }, [serverId]);

  return <div ref={box} className={styles.screen} aria-label={t("remote.terminal.label")} role="application" />;
}

/** What a recorded session printed, in a terminal that takes no input. */
export function RecordingView({ sessionId, onClose }: { sessionId: string; onClose: () => void }) {
  const box = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    const element = box.current;
    if (!element) return;
    const controller = new AbortController();
    let dispose: (() => void) | null = null;
    void (async () => {
      let recording: RemoteRecording;
      try {
        recording = await api.remoteRecording(sessionId, controller.signal);
      } catch (e) {
        if (!controller.signal.aborted) setError(describeError(e));
        setLoading(false);
        return;
      }
      try {
        const loaded = await loadTerminal(element, true);
        if (controller.signal.aborted) return loaded.dispose();
        dispose = loaded.dispose;
        for (const chunk of recording.chunks) loaded.terminal.write(decodeChunk(chunk.data));
      } catch (e) {
        setError(t("remote.terminal.cannotLoad", { error: describeError(e) }));
      }
      setLoading(false);
    })();
    return () => {
      controller.abort();
      dispose?.();
    };
  }, [sessionId]);

  return (
    <div className={styles.recording}>
      <div className={styles.bar}>
        <span className={styles.status}>
          {error ?? (loading ? t("remote.recording.loading") : t("remote.recording.title"))}
        </span>
        <Button size="small" onClick={onClose}>
          {t("remote.recording.close")}
        </Button>
      </div>
      <div ref={box} className={styles.screen} aria-label={t("remote.recording.label")} />
    </div>
  );
}
