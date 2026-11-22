"use client";

import { useState } from "react";

import { t } from "@/i18n";
import type { SilenceRequest } from "@/lib/api";
import { defaultReason, inOneHour, tomorrowMorning } from "@/lib/silences";
import { RowMenu } from "@/components/ui/RowMenu";

import { SilenceForm } from "./Silences";
import styles from "./BulkSilence.module.css";

/** A silence's period, as the unit route takes it. */
export type SilencePeriod = Pick<SilenceRequest, "starts_at" | "ends_at" | "reason">;

/**
 * "Silence ▾" of a bulk bar: 1 hour, until tomorrow 8:00, or a custom
 * period in a form under the bar, as on a server's page. `onSilence`
 * resolves once every item had its turn.
 */
export function BulkSilence({
  label,
  submitLabel,
  disabled,
  onSilence,
}: {
  /** Accessible name of the menu: "Silence the servers of the 3 selected incidents". */
  label: string;
  submitLabel: string;
  disabled?: boolean;
  onSilence: (period: SilencePeriod) => Promise<void>;
}) {
  const [custom, setCustom] = useState(false);
  const quick = (end: Date) => void onSilence({ ends_at: end.toISOString(), reason: defaultReason() });
  return (
    <>
      <RowMenu
        label={label}
        trigger={t("silences.bar.trigger")}
        disabled={disabled}
        items={[
          { label: t("silences.bar.hour"), onSelect: () => quick(inOneHour(new Date())) },
          { label: t("silences.bar.tomorrow"), onSelect: () => quick(tomorrowMorning(new Date())) },
          { label: t("silences.bar.custom"), onSelect: () => setCustom(true) },
        ]}
      />
      {custom && (
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
      )}
    </>
  );
}
