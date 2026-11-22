"use client";

import { type ReactNode, useState } from "react";

import { t } from "@/i18n";
import type { SilenceRequest } from "@/lib/api";
import { defaultReason, inOneHour, tomorrowMorning } from "@/lib/silences";
import { RowMenu, type RowMenuItem } from "@/components/ui/RowMenu";

import { SilenceForm } from "./Silences";
import styles from "./BulkSilence.module.css";

/** A silence's period, as the unit route takes it. */
export type SilencePeriod = Pick<SilenceRequest, "starts_at" | "ends_at" | "reason">;

/**
 * The silence choices of a bulk bar: 1 hour, until tomorrow 8:00, or a
 * custom period in a form under the bar, as on a server's page. `word`
 * names what is silenced when the choices sit in a shared "⋯" menu.
 * `onSilence` resolves once every item had its turn.
 */
export function useBulkSilence(
  submitLabel: string,
  onSilence: (period: SilencePeriod) => Promise<void>,
  word: (choice: string) => string = (choice) => choice,
): { items: RowMenuItem[]; form: ReactNode } {
  const [custom, setCustom] = useState(false);
  const quick = (end: Date) => void onSilence({ ends_at: end.toISOString(), reason: defaultReason() });
  const items: RowMenuItem[] = [
    { label: word(t("silences.bar.hour")), onSelect: () => quick(inOneHour(new Date())) },
    { label: word(t("silences.bar.tomorrow")), onSelect: () => quick(tomorrowMorning(new Date())) },
    { label: word(t("silences.bar.custom")), onSelect: () => setCustom(true) },
  ];
  const form = custom && (
    <div className={styles.form}>
      <SilenceForm
        submitLabel={submitLabel}
        onSubmit={async (period) => {
          await onSilence(period);
          setCustom(false);
        }}
        onCancel={() => setCustom(false)}
      />
    </div>
  );
  return { items, form };
}

/** "Silence ▾" of a bulk bar, with the choices of `useBulkSilence`. */
export function BulkSilence({
  label,
  submitLabel,
  disabled,
  onSilence,
}: {
  /** Accessible name of the menu: "Silence the 3 selected servers". */
  label: string;
  submitLabel: string;
  disabled?: boolean;
  onSilence: (period: SilencePeriod) => Promise<void>;
}) {
  const { items, form } = useBulkSilence(submitLabel, onSilence);
  return (
    <>
      <RowMenu label={label} trigger={t("silences.bar.trigger")} disabled={disabled} items={items} />
      {form}
    </>
  );
}
