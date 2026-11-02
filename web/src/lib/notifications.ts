import type { Customer, Me, NotificationChannel, Site } from "./api";
import { administersCustomer, administersEverything, administersSite } from "./me";

/** One choice of "Notify about" in the channel form (ADR-0078). */
export interface ChannelScope {
  value: string;
  label: string;
  /** What it means, in plain words. */
  note: string;
  customer_id: string | null;
  site_id: string | null;
}

const EVERYTHING: ChannelScope = {
  value: "everything",
  label: "Everything",
  note: "Every alert, and news about the central server: failed backups, automatic updates, new versions.",
  customer_id: null,
  site_id: null,
};

function customerScope(id: string, name: string): ChannelScope {
  return {
    value: `customer:${id}`,
    label: `All of ${name}`,
    note: `Alerts from ${name} and all its sites. Nothing from other customers.`,
    customer_id: id,
    site_id: null,
  };
}

function siteScope(id: string, name: string, customer: string, named: boolean): ChannelScope {
  return {
    value: `site:${id}`,
    label: named ? `${customer} / ${name}` : name,
    note: `Alerts from ${name} only.`,
    customer_id: null,
    site_id: id,
  };
}

/** The choice a saved channel holds. */
export function scopeValue(channel: NotificationChannel): string {
  if (channel.site_id) return `site:${channel.site_id}`;
  return channel.customer_id ? `customer:${channel.customer_id}` : EVERYTHING.value;
}

/** What a channel is about, for the list; the customer only among several. */
export function scopeName(channel: NotificationChannel, severalCustomers: boolean): string {
  if (channel.site_id) {
    const site = channel.site_name ?? "";
    return severalCustomers ? `${channel.customer_name} / ${site}` : site;
  }
  return channel.customer_name ?? EVERYTHING.label;
}

/**
 * What `me` may point a channel at, widest first. An install with one
 * customer does not hear of customers, nor one with one site of sites.
 * `current` stays a choice whatever the rules say, so editing never moves
 * a channel by itself. UX only: the API decides.
 */
export function channelScopes(
  me: Me | null,
  customers: Customer[],
  sites: Site[],
  current?: NotificationChannel,
): ChannelScope[] {
  const global = administersEverything(me);
  const several = customers.length > 1;
  const scopes = global ? [EVERYTHING] : [];
  for (const customer of customers) {
    if ((several || !global) && administersCustomer(me, customer.id)) {
      scopes.push(customerScope(customer.id, customer.name));
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
      scopes.push(customerScope(current.customer_id, customer));
    } else {
      scopes.unshift(EVERYTHING);
    }
  }
  return scopes;
}
