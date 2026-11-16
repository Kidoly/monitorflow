/**
 * Customer wording, shown only to someone who sees several customers
 * (`useMultiCustomer`, ADR-0104). Without them, the one customer is
 * implicit: a site is named alone, and a choice covering the customer as a
 * whole covers "All sites".
 */

import { t } from "@/i18n";

/** In English, for comparisons; the screen says `allSites()`. */
export const ALL_SITES = "All sites";

/** "All sites" in the active language. */
export const allSites = () => t("ui.allSites");

/** "acme › paris" among several customers, "paris" otherwise. */
export function placeLabel(
  customer: string | null | undefined,
  site: string,
  multiCustomer: boolean,
  separator = " › ",
): string {
  return multiCustomer && customer ? `${customer}${separator}${site}` : site;
}

/** `label` ("All of acme", "Customer acme") among several customers, "All sites" otherwise. */
export function customerWide(label: string, multiCustomer: boolean): string {
  return multiCustomer ? label : allSites();
}
