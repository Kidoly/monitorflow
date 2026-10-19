import type { IncidentSeverity, IncidentState } from "@/lib/api";
import { SEVERITY_LABELS } from "@/lib/thresholds";

import styles from "./IncidentBadges.module.css";

/** The severity in words, with a shape so it does not rely on colour. */
export function SeverityBadge({ severity, state }: { severity: IncidentSeverity; state?: IncidentState }) {
  const resolved = state === "resolved";
  return (
    <span className={resolved ? styles.resolved : styles[severity]}>
      <span aria-hidden="true">{severity === "critical" ? "◆" : "▲"}</span> {SEVERITY_LABELS[severity]}
    </span>
  );
}
