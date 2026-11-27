"use client";

import { useCallback, useEffect, useRef, useState } from "react";

import { t } from "@/i18n";
import { type RemoteRecording, api, describeError } from "@/lib/api";
import { clock, framesToDraw } from "@/lib/remote";
import { Button, fieldClass } from "@/components/ui/Button";

import styles from "./DesktopRecording.module.css";
import terminalStyles from "./RemoteTerminal.module.css";

/** Pictures fetched ahead of the one drawn. */
const AHEAD = 8;
const SPEEDS = [1, 2, 4, 8];

/**
 * The replay of a recorded desktop (ADR-0192): the pictures the central server kept of its screen,
 * each drawn at its place from a picture of the whole screen on, with play, pause, a position and a
 * speed. The canvas scales to the page's width, without scrollbars.
 */
export function DesktopRecordingView({ sessionId, onClose }: { sessionId: string; onClose: () => void }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const [recording, setRecording] = useState<RemoteRecording | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [position, setPosition] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [speed, setSpeed] = useState(1);
  // What the canvas shows: the index of the last picture drawn.
  const drawn = useRef(-1);
  const wanted = useRef(0);
  const pictures = useRef(new Map<number, Promise<ImageBitmap>>());
  const queue = useRef<Promise<void>>(Promise.resolve());

  useEffect(() => {
    const controller = new AbortController();
    api
      .remoteRecording(sessionId, controller.signal)
      .then(setRecording)
      .catch((e) => {
        if (!controller.signal.aborted) setError(describeError(e));
      });
    return () => controller.abort();
  }, [sessionId]);

  const frames = recording?.frames ?? [];
  const last = frames.at(-1)?.at_ms ?? 0;
  const length = Math.max(last, (recording?.session.duration_seconds ?? 0) * 1000);

  const picture = useCallback(
    (seq: number) => {
      let found = pictures.current.get(seq);
      if (!found) {
        found = api.remoteDesktopFrame(sessionId, seq).then((blob) => createImageBitmap(blob));
        // A failed fetch is tried again next time.
        found.catch(() => pictures.current.delete(seq));
        pictures.current.set(seq, found);
      }
      return found;
    },
    [sessionId],
  );

  // Draws up to `position`, one step after another, so pictures land in order.
  useEffect(() => {
    wanted.current = position;
    if (!recording) return;
    queue.current = queue.current.then(async () => {
      const context = canvas.current?.getContext("2d");
      if (!context || wanted.current !== position) return;
      const { draw, drawn: next } = framesToDraw(recording.frames, drawn.current, position);
      for (const frame of recording.frames.slice(next + 1, next + 1 + AHEAD)) void picture(frame.seq).catch(() => {});
      try {
        for (const frame of draw) {
          const bitmap = await picture(frame.seq);
          const target = context.canvas;
          if (target.width !== frame.screen_width || target.height !== frame.screen_height) {
            target.width = frame.screen_width;
            target.height = frame.screen_height;
          }
          context.drawImage(bitmap, frame.x, frame.y);
        }
        drawn.current = next;
      } catch (e) {
        setError(describeError(e));
        setPlaying(false);
      }
    });
  }, [recording, position, picture]);

  useEffect(() => {
    if (!playing) return;
    const from = position;
    const start = performance.now();
    const timer = window.setInterval(() => {
      const at = from + (performance.now() - start) * speed;
      if (at >= length) {
        setPosition(length);
        setPlaying(false);
      } else {
        setPosition(at);
      }
    }, 100);
    return () => window.clearInterval(timer);
    // The position moves on by itself while playing: start again only on a new speed or state.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playing, speed, length]);

  const status =
    error ??
    (recording === null
      ? t("remote.desktopRecording.loading")
      : frames.length === 0
        ? t("remote.desktopRecording.empty")
        : t("remote.desktopRecording.title"));
  const ready = recording !== null && frames.length > 0 && error === null;

  return (
    <div className={terminalStyles.recording}>
      <div className={terminalStyles.bar}>
        <span className={terminalStyles.status} role="status">
          {status}
        </span>
        <Button size="small" onClick={onClose}>
          {t("remote.recording.close")}
        </Button>
      </div>
      {ready && (
        <div className={styles.controls}>
          <Button
            size="small"
            variant="k"
            onClick={() => {
              if (!playing && position >= length) setPosition(0);
              setPlaying(!playing);
            }}
          >
            {playing ? t("remote.desktopRecording.pause") : t("remote.desktopRecording.play")}
          </Button>
          <input
            className={styles.position}
            type="range"
            min={0}
            max={length}
            step={100}
            value={position}
            aria-label={t("remote.desktopRecording.position")}
            onChange={(e) => setPosition(Number(e.target.value))}
          />
          <span className={terminalStyles.status}>
            {t("remote.desktopRecording.time", { at: clock(position), length: clock(length) })}
          </span>
          <select
            className={fieldClass}
            value={speed}
            aria-label={t("remote.desktopRecording.speed")}
            onChange={(e) => setSpeed(Number(e.target.value))}
          >
            {SPEEDS.map((s) => (
              <option key={s} value={s}>
                {`${s}×`}
              </option>
            ))}
          </select>
        </div>
      )}
      {recording !== null && frames.length > 0 && (
        <canvas ref={canvas} className={styles.screen} aria-label={t("remote.desktopRecording.label")} />
      )}
    </div>
  );
}
