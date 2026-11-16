import type { LicenseView } from "./api";
import type { Tone } from "@/components/ui/tone";
import { t } from "@/i18n";
import { dateLocale } from "@/i18n/locale";

/** `2027-11-30` as "30 November 2027", "30 novembre 2027": licenses count in whole UTC days. */
export function formatDay(day: string): string {
  return new Date(`${day}T00:00:00Z`).toLocaleDateString(dateLocale(), {
    day: "numeric",
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  });
}

/** "3 central servers", the first one included. */
export function centralServers(count: number): string {
  return t("license.centralServers", { count });
}

/** One line for where the license stands, and its tint (ADR-0100). */
export function licenseSummary(view: LicenseView): [string, Tone] {
  const license = view.license;
  switch (view.state) {
    case "valid":
      return license
        ? [t("license.summary.valid", { day: formatDay(license.expires), servers: centralServers(license.server_instances) }), "ok"]
        : [t("license.summary.validShort"), "ok"];
    case "expires_soon":
      return license
        ? [t("license.summary.expiresSoon", { day: formatDay(license.expires) }), "warn"]
        : [t("license.summary.expiresSoonShort"), "warn"];
    case "expired":
      return [
        license ? t("license.summary.expiredOn", { day: formatDay(license.expires) }) : t("license.summary.expired"),
        "bad",
      ];
    case "invalid":
      // The problem is the server's words, in English (ADR-0136).
      return [t("license.summary.invalid", { problem: view.problem ?? t("license.summary.unknownReason") }), "bad"];
    default:
      return [t("license.summary.none"), "neutral"];
  }
}

/**
 * What global administrators are told on every page (ADR-0100): only once
 * a central server was added, and only when the license stops covering
 * more. A single central server never sees one.
 */
export function licenseNotice(view: LicenseView): string | null {
  const added = view.added?.length ?? 0;
  if (!view.available || added === 0) return null;
  const keep = t("license.notice.keep", { count: added });
  const expires = view.license ? formatDay(view.license.expires) : null;
  switch (view.state) {
    case "expires_soon":
      return t("license.notice.expiresSoon", { day: expires ?? "", keep });
    case "expired":
      return expires ? t("license.notice.expiredOn", { day: expires, keep }) : t("license.notice.expired", { keep });
    case "invalid":
      return t("license.notice.invalid", { keep });
    case "none":
      return t("license.notice.none", { keep });
    default:
      return null;
  }
}
