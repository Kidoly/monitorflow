import type { License, LicenseTier, LicenseView } from "./api";
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

/** The tier of a key; keys from before ADR-0147 are all Enterprise. */
export function tierOf(license: License): LicenseTier {
  return license.tier ?? "enterprise";
}

/** "Standard", as people read it. */
export function tierName(tier: LicenseTier): string {
  return t(`license.tier.${tier}`);
}

/** Whether the key allows more than one central server: only then is high availability worth a word. */
export function addsCentralServers(license: License): boolean {
  return tierOf(license) === "enterprise" && license.server_instances > 1;
}

/** What a tier includes, in plain words, each line one thing (ADR-0147). */
export function tierIncludes(tier: LicenseTier): string[] {
  const lines = [t("license.includes.everything"), t("license.includes.tested"), t(`license.includes.support.${tier}`)];
  if (tier === "enterprise") lines.push(t("license.includes.centralServers"));
  return lines;
}

/** The link of a support contact: an email address or an `https://` address, nothing else. */
export function supportHref(contact: string): string | null {
  if (/^https:\/\/\S+$/.test(contact)) return contact;
  if (/^[^\s@:<>"'`]+@[^\s@:<>"'`]+\.[^\s@:<>"'`]+$/.test(contact)) return `mailto:${contact}`;
  return null;
}

/**
 * The calm note when more servers are managed than the key covers
 * (ADR-0147): nothing stops, it only says so, on the License page alone.
 */
export function coverageNote(view: LicenseView): string | null {
  const covered = view.license?.managed_servers;
  const inUse = view.managed_servers_in_use;
  if (covered == null || inUse == null || inUse <= covered) return null;
  if (view.state !== "valid" && view.state !== "expires_soon") return null;
  return t("license.coverage.over", { count: inUse, covered });
}

/** One line for where the license stands, and its tint (ADR-0100, ADR-0147). */
export function licenseSummary(view: LicenseView): [string, Tone] {
  const license = view.license;
  const tier = license ? tierName(tierOf(license)) : "";
  const central = license != null && addsCentralServers(license);
  switch (view.state) {
    case "valid":
      return license
        ? [
            central
              ? t("license.summary.validCentral", { tier, day: formatDay(license.expires), servers: centralServers(license.server_instances) })
              : t("license.summary.valid", { tier, day: formatDay(license.expires) }),
            "ok",
          ]
        : [t("license.summary.validShort"), "ok"];
    case "expires_soon":
      return license
        ? [
            t(central ? "license.summary.expiresSoonCentral" : "license.summary.expiresSoon", { tier, day: formatDay(license.expires) }),
            // Calm unless central servers depend on it (ADR-0147).
            central ? "warn" : "neutral",
          ]
        : [t("license.summary.expiresSoonShort"), "warn"];
    case "expired":
      return [
        license
          ? t(central ? "license.summary.expiredOnCentral" : "license.summary.expiredOn", { tier, day: formatDay(license.expires) })
          : t("license.summary.expired"),
        license && !central ? "neutral" : "bad",
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
  const plan = view.license ? tierOf(view.license) : "enterprise";
  if ((view.state === "valid" || view.state === "expires_soon") && plan !== "enterprise") {
    return t("license.notice.notEnterprise", { tier: tierName(plan), keep });
  }
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
