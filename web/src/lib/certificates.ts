/** Days before expiry from which a certificate is shown as expiring. */
export const CERT_WARNING_DAYS = 14;

export type CertificateState = "valid" | "expiring" | "expired";

/** Whether a certificate expiring at `notAfter` needs attention at `now`. */
export function certificateState(notAfter: string, now: Date): CertificateState {
  const left = Date.parse(notAfter) - now.getTime();
  if (left <= 0) return "expired";
  return left < CERT_WARNING_DAYS * 86_400_000 ? "expiring" : "valid";
}
