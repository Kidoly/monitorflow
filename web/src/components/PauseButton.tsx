"use client";

import { type FormEvent, useEffect, useRef, useState } from "react";

import { t } from "@/i18n";
import type { SilenceRequest } from "@/lib/api";
import { MAX_DAYS, fromLocalInput, inOneHour, tomorrowMorning } from "@/lib/silences";
import { Button, buttonClass, fieldClass } from "@/components/ui/Button";
import { cx } from "@/components/ui/cx";

import styles from "./AcknowledgeButton.module.css";

/** A pause's explanation and end, as `POST /api/silences` takes them. */
export type Pause = Pick<SilenceRequest, "ends_at" | "reason">;

const HOUR_MS = 3_600_000;
const DAY_MS = 24 * HOUR_MS;

/** How long a pause lasts; every silence ends on its own, 30 days at most (ADR-0079). */
const LENGTHS = ["hour", "tomorrow", "week", "max", "until"] as const;
type Length = (typeof LENGTHS)[number];

/**
 * The end of a pause of `length` from `now`; `until` is the date typed.
 * The longest stops a few minutes short of 30 days, so a browser clock a
 * little ahead of the server's is not refused.
 */
export function pauseEnd(length: Length, now: Date, until: string): Date | null {
  switch (length) {
    case "hour":
      return inOneHour(now);
    case "tomorrow":
      return tomorrowMorning(now);
    case "week":
      return new Date(now.getTime() + 7 * DAY_MS);
    case "max":
      return new Date(now.getTime() + MAX_DAYS * DAY_MS - 5 * 60_000);
    case "until":
      return fromLocalInput(until);
  }
}

/**
 * "Pause", then in place, as Acknowledge is: why (required, so others
 * know), for how long (1 hour to 30 days, or until a date), Cancel and
 * Pause. A pause silences the chosen sensors' alerts; they stay measured
 * (ADR-0151). Escape closes it; `onPause` rejecting keeps it open.
 */
export function PauseButton({
  name,
  disabled,
  onPause,
  describeError,
}: {
  /** Accessible name of the trigger: "Pause the 3 selected sensors". */
  name: string;
  disabled?: boolean;
  onPause: (pause: Pause) => Promise<void>;
  describeError: (error: unknown) => string;
}) {
  const [open, setOpen] = useState(false);
  const [reason, setReason] = useState("");
  const [length, setLength] = useState<Length>("hour");
  const [until, setUntil] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const moved = useRef<"open" | "closed" | null>(null);

  useEffect(() => {
    if (moved.current === "closed") {
      moved.current = null;
      trigger.current?.focus();
    }
  }, [open]);

  const field = (input: HTMLInputElement | null) => {
    if (input && moved.current === "open") {
      moved.current = null;
      input.focus();
    }
  };

  function close() {
    moved.current = "closed";
    setOpen(false);
    setReason("");
    setLength("hour");
    setUntil("");
    setError(null);
  }

  async function submit(event: FormEvent) {
    event.preventDefault();
    const end = pauseEnd(length, new Date(), until);
    if (!reason.trim()) return setError(t("silences.pause.noReason"));
    if (!end) return setError(t("silences.form.noEnd"));
    setBusy(true);
    setError(null);
    try {
      await onPause({ reason: reason.trim(), ends_at: end.toISOString() });
      close();
    } catch (e) {
      setError(describeError(e));
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <Button
        ref={trigger}
        size="small"
        aria-label={name}
        disabled={disabled}
        onClick={() => {
          moved.current = "open";
          setOpen(true);
        }}
      >
        {t("silences.pause.trigger")}
      </Button>
    );
  }
  return (
    <form
      className={styles.form}
      aria-label={name}
      onSubmit={(e) => void submit(e)}
      onKeyDown={(e) => {
        if (e.key === "Escape" && !busy) close();
      }}
    >
      <label className={styles.label}>
        <span>{t("silences.pause.why")}</span>
        <input
          ref={field}
          className={cx(fieldClass, styles.field)}
          value={reason}
          maxLength={200}
          placeholder={t("silences.pause.placeholder")}
          required
          disabled={busy}
          onChange={(e) => setReason(e.target.value)}
        />
      </label>
      <label className={styles.label}>
        <span>{t("silences.pause.for")}</span>
        <select className={fieldClass} value={length} disabled={busy} onChange={(e) => setLength(e.target.value as Length)}>
          {LENGTHS.map((l) => (
            <option key={l} value={l}>
              {t(`silences.pause.length.${l}`)}
            </option>
          ))}
        </select>
      </label>
      {length === "until" && (
        <label className={styles.label}>
          <span>{t("silences.pause.until", { count: MAX_DAYS })}</span>
          <input className={fieldClass} type="datetime-local" required value={until} disabled={busy} onChange={(e) => setUntil(e.target.value)} />
        </label>
      )}
      <Button size="small" onClick={close} disabled={busy}>
        {t("silences.form.cancel")}
      </Button>
      <button type="submit" className={buttonClass({ variant: "k", size: "small" })} disabled={busy}>
        {busy ? t("silences.form.saving") : t("silences.pause.submit")}
      </button>
      {error && (
        <span className={styles.error} role="alert">
          {error}
        </span>
      )}
    </form>
  );
}
