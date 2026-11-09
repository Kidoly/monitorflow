import type { LicenseView } from "./api";
import type { Tone } from "@/components/ui/tone";

/** `2027-11-30` as "30 November 2027": licenses count in whole UTC days. */
export function formatDay(day: string): string {
  return new Date(`${day}T00:00:00Z`).toLocaleDateString("en-GB", {
    day: "numeric",
    month: "long",
    year: "numeric",
    timeZone: "UTC",
  });
}

/** "3 central servers", the first one included. */
export function centralServers(count: number): string {
  return count === 1 ? "1 central server" : `${count} central servers`;
}

/** One line for where the license stands, and its tint (ADR-0100). */
export function licenseSummary(view: LicenseView): [string, Tone] {
  const license = view.license;
  switch (view.state) {
    case "valid":
      return license ? [`Valid until ${formatDay(license.expires)}, for ${centralServers(license.server_instances)}.`, "ok"] : ["Valid.", "ok"];
    case "expires_soon":
      return license
        ? [`Expires on ${formatDay(license.expires)}. Central servers already added keep running; after that day no other can be added.`, "warn"]
        : ["Expires soon.", "warn"];
    case "expired":
      return [
        `Expired${license ? ` on ${formatDay(license.expires)}` : ""}. Central servers already added keep running; none can be added until a new license key is installed.`,
        "bad",
      ];
    case "invalid":
      return [`The license key no longer verifies: ${view.problem ?? "unknown reason"}.`, "bad"];
    default:
      return ["No license key.", "neutral"];
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
  const keep = `${added === 1 ? "The central server" : `The ${added} central servers`} already added keep${added === 1 ? "s" : ""} running`;
  const expires = view.license ? formatDay(view.license.expires) : null;
  switch (view.state) {
    case "expires_soon":
      return `The Enterprise license expires on ${expires}. ${keep}, but no other can be added after that day.`;
    case "expired":
      return `The Enterprise license expired${expires ? ` on ${expires}` : ""}. ${keep}, but no other can be added.`;
    case "invalid":
      return `The Enterprise license key no longer verifies. ${keep}, but no other can be added.`;
    case "none":
      return `There is no Enterprise license key. ${keep}, but no other can be added.`;
    default:
      return null;
  }
}
