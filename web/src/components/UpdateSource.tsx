import type { WindowsUpdateSource } from "@/lib/api";

import styles from "./PackageSources.module.css";
import admin from "./AdminPages.module.css";
import { Banner } from "./ui/PageState";
import { Label } from "./ui/StatusLabel";
import type { Tone } from "./ui/tone";

/** What a Windows server's update source says, in plain words (ADR-0086). */
export interface UpdateSourceWords {
  /** One to four words for a label. */
  label: string;
  tone: Tone;
  /** Where updates come from, in a sentence. */
  text: string;
  /** Settings that contradict each other or keep updates away. */
  warnings: string[];
  /** The rest: other servers, automatic updates, deferrals. */
  details: string[];
}

const NOT_KNOWN: UpdateSourceWords = {
  label: "Not known yet",
  tone: "neutral",
  text: "Where its updates come from isn't known yet. It appears with the next list of updates from an up-to-date agent.",
  warnings: [],
  details: [],
};

const AUTOMATIC: Record<NonNullable<WindowsUpdateSource["automatic"]>, string> = {
  off: "Automatic updates are off.",
  notify: "Windows only says when updates are available: it doesn't download them on its own.",
  download: "Windows downloads updates on its own and waits for them to be installed.",
  install: "Windows installs updates on its own schedule, outside update campaigns.",
  local_choice: "Its local administrators choose how Windows installs updates.",
};

const days = (n: number) => `${n} ${n === 1 ? "day" : "days"}`;

/** The kinds of updates `source` takes from Microsoft while on WSUS. */
function fromMicrosoft(source: WindowsUpdateSource): string | null {
  if (source.quality_from_microsoft && source.feature_from_microsoft) return "Monthly and feature updates";
  if (source.quality_from_microsoft) return "Monthly updates";
  if (source.feature_from_microsoft) return "Feature updates";
  return null;
}

export function describeUpdateSource(source: WindowsUpdateSource | null): UpdateSourceWords {
  if (!source) return NOT_KNOWN;
  const warnings: string[] = [];
  const details: string[] = [];
  let words: Pick<UpdateSourceWords, "label" | "tone" | "text">;
  if (source.origin === "wsus") {
    words = { label: "WSUS", tone: "ok", text: `Updates come from WSUS at ${source.wsus_url}.` };
  } else if (source.origin === "wsus_without_url") {
    words = { label: "WSUS without a server", tone: "warn", text: "Policy turns WSUS on but names no WSUS server." };
    warnings.push(
      'Windows may find no updates. Set "Specify intranet Microsoft update service location" in its Windows Update policy.',
    );
  } else {
    words = { label: "Microsoft", tone: "neutral", text: "Updates come from Microsoft (Windows Update)." };
    if (source.wsus_url) {
      warnings.push(`Policy names the WSUS server ${source.wsus_url} but doesn't turn it on, so Windows ignores it.`);
    }
    if (!source.managed) details.push("No Windows Update policy is set: Windows uses its own defaults.");
  }
  const microsoft = fromMicrosoft(source);
  if (microsoft) {
    details.push(`${microsoft} come from Microsoft instead, by policy.`);
    if (source.no_internet) {
      warnings.push(`${microsoft} come from Microsoft, but policy also forbids Windows Update Internet locations.`);
    }
  }
  if (source.download_url) details.push(`Downloads come from ${source.download_url}.`);
  if (source.status_url) details.push(`It reports its status to ${source.status_url}.`);
  if (source.automatic) details.push(AUTOMATIC[source.automatic]);
  const deferrals = [
    source.quality_deferral_days > 0 && `monthly updates ${days(source.quality_deferral_days)}`,
    source.feature_deferral_days > 0 && `feature updates ${days(source.feature_deferral_days)}`,
  ].filter(Boolean);
  if (deferrals.length > 0) {
    details.push(`After Microsoft releases them, Windows waits before offering ${deferrals.join(" and ")}.`);
  }
  return { ...words, warnings, details };
}

/** The section of a Windows server's Updates tab, in place of its package sources. */
export function UpdateSource({ source }: { source: WindowsUpdateSource | null }) {
  const words = describeUpdateSource(source);
  return (
    <section className={admin.section} aria-labelledby="update-source-title">
      <div className={admin.sectionHead}>
        <h2 id="update-source-title" className={admin.sectionTitle}>
          Update source
        </h2>
        <Label tone={words.tone}>{words.label}</Label>
      </div>
      <div className={styles.body}>
        <p className={styles.note}>{words.text}</p>
        {words.warnings.map((w) => (
          <Banner key={w} tone="warn">
            {w}
          </Banner>
        ))}
        {words.details.map((d) => (
          <p key={d} className={styles.note}>
            {d}
          </p>
        ))}
      </div>
    </section>
  );
}
