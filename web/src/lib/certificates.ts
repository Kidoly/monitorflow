/**
 * Days left under which renewal counts as late. Peers renew once two thirds
 * of their 7 days have passed, about 2.3 days before expiry (ADR-0010), so a
 * healthy certificate never gets here.
 */
export const RENEWAL_LATE_DAYS = 2;

export type CertificateState = "valid" | "late" | "failed";

/** Whether renewal of a certificate expiring at `notAfter` needs attention at `now`: late, or failed once it expired. */
export function certificateState(notAfter: string, now: Date): CertificateState {
  const left = Date.parse(notAfter) - now.getTime();
  if (left <= 0) return "failed";
  return left < RENEWAL_LATE_DAYS * 86_400_000 ? "late" : "valid";
}

const days = (n: number) => `${n} ${n === 1 ? "day" : "days"}`;

/** "Renewal late, expires in 1 day" or "Renewal failed, expired 3 days ago"; null while `valid`. */
export function certificateProblem(notAfter: string, now: Date): string | null {
  const state = certificateState(notAfter, now);
  if (state === "valid") return null;
  const left = Math.abs(Date.parse(notAfter) - now.getTime()) / 86_400_000;
  if (state === "late") return `Renewal late, expires in ${left < 1 ? "under a day" : days(Math.round(left))}`;
  return `Renewal failed, expired ${left < 1 ? "today" : `${days(Math.round(left))} ago`}`;
}
