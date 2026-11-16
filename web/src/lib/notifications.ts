import type { Customer, Me, NotificationChannel, Site } from "./api";
import { t } from "@/i18n";

import { administersCustomer, administersEverything, administersSite, multiCustomer } from "./me";

/** One choice of "Notify about" in the channel form (ADR-0078). */
export interface ChannelScope {
  value: string;
  label: string;
  /** What it means, in plain words. */
  note: string;
  /** What it covers inside a sentence: "everything", "acme", "all sites", "acme / paris". */
  place: string;
  customer_id: string | null;
  site_id: string | null;
}

const EVERYTHING_VALUE = "everything";

/** "Everything", in the active language. */
function everything(): ChannelScope {
  return {
    value: EVERYTHING_VALUE,
    label: t("notifications.scope.everything"),
    note: t("notifications.scope.everythingNote"),
    place: t("notifications.scope.everythingPlace"),
    customer_id: null,
    site_id: null,
  };
}

/** A whole customer; "All sites" for someone who sees one customer (ADR-0104). */
function customerScope(id: string, name: string, named: boolean): ChannelScope {
  return {
    value: `customer:${id}`,
    label: named ? t("notifications.scope.allOf", { name }) : t("notifications.scope.allSites"),
    note: named ? t("notifications.scope.customerNote", { name }) : t("notifications.scope.allSitesNote"),
    place: named ? name : t("notifications.scope.allSitesPlace"),
    customer_id: id,
    site_id: null,
  };
}

function siteScope(id: string, name: string, customer: string, named: boolean): ChannelScope {
  return {
    value: `site:${id}`,
    label: named ? `${customer} / ${name}` : name,
    note: t("notifications.scope.siteNote", { name }),
    place: named ? `${customer} / ${name}` : name,
    customer_id: null,
    site_id: id,
  };
}

/** What a saved channel or schedule covers. */
type Covered = Pick<NotificationChannel, "customer_id" | "customer_name" | "site_id" | "site_name">;

/** The choice a saved channel or schedule holds. */
export function scopeValue(channel: Covered): string {
  if (channel.site_id) return `site:${channel.site_id}`;
  return channel.customer_id ? `customer:${channel.customer_id}` : EVERYTHING_VALUE;
}

/** What a channel is about, for the list; the customer only among several. */
export function scopeName(channel: Covered, severalCustomers: boolean): string {
  if (channel.site_id) {
    const site = channel.site_name ?? "";
    return severalCustomers ? `${channel.customer_name} / ${site}` : site;
  }
  if (channel.customer_id && !severalCustomers) return t("notifications.scope.allSites");
  return channel.customer_name ?? t("notifications.scope.everything");
}

/**
 * What `me` may point a channel at, widest first. Someone who sees one
 * customer does not hear of customers (ADR-0104), nor one with one site of
 * sites.
 * `current` stays a choice whatever the rules say, so editing never moves
 * a channel by itself. UX only: the API decides.
 */
export function channelScopes(
  me: Me | null,
  customers: Customer[],
  sites: Site[],
  current?: Covered,
): ChannelScope[] {
  const global = administersEverything(me);
  const several = multiCustomer(me);
  const scopes = global ? [everything()] : [];
  for (const customer of customers) {
    if ((several || !global) && administersCustomer(me, customer.id)) {
      scopes.push(customerScope(customer.id, customer.name, several));
    }
    if (several || sites.length > 1 || !global) {
      for (const site of sites) {
        if (site.customer_id === customer.id && administersSite(me, customer.id, site.id)) {
          scopes.push(siteScope(site.id, site.name, customer.name, several));
        }
      }
    }
  }
  if (current && !scopes.some((scope) => scope.value === scopeValue(current))) {
    const customer = current.customer_name ?? "";
    if (current.site_id) {
      scopes.push(siteScope(current.site_id, current.site_name ?? "", customer, several));
    } else if (current.customer_id) {
      scopes.push(customerScope(current.customer_id, customer, several));
    } else {
      scopes.unshift(everything());
    }
  }
  return scopes;
}
