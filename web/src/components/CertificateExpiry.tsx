import { certificateState } from "@/lib/certificates";
import { formatDateTime, formatRelative } from "@/lib/format";

import styles from "./CertificateExpiry.module.css";

/**
 * When a certificate expires, relative to `now`. Under
 * `CERT_WARNING_DAYS` days it is a warning, past it an error.
 * `compact` shows nothing for a valid certificate, for tables.
 */
export function CertificateExpiry({ notAfter, now, compact = false }: { notAfter: string; now: Date; compact?: boolean }) {
  const state = certificateState(notAfter, now);
  if (compact && state === "valid") return null;
  const time = (
    <time dateTime={notAfter} title={formatDateTime(notAfter)}>
      {formatRelative(notAfter, now)}
    </time>
  );
  switch (state) {
    case "valid":
      return time;
    case "expiring":
      return (
        <span className={styles.expiring}>
          <span aria-hidden="true">⚠</span> {compact ? "Certificate expires " : "Expires "}
          {time}
        </span>
      );
    case "expired":
      return (
        <span className={styles.expired}>
          <span aria-hidden="true">⚠</span> {compact ? "Certificate expired " : "Expired "}
          {time}
        </span>
      );
  }
}
