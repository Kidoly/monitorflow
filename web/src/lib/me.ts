import { createContext, useContext } from "react";

import type { Me } from "./api";

/** The signed-in user, provided by `AuthGuard`; null outside it. */
export const MeContext = createContext<Me | null>(null);

export function useMe(): Me | null {
  return useContext(MeContext);
}

/**
 * Whether `me` has an administrator grant of any kind, which shows the
 * Administration section. UX only: the API decides (ADR-0029).
 */
export function isAdministrator(me: Me | null): boolean {
  return me?.roles.some((grant) => grant.role === "administrator") ?? false;
}

/**
 * Whether `me` may enroll and revoke peers of `customerId`: an administrator
 * grant that is global or on that customer. Site grants are not enough.
 * Without `customerId`, on any customer.
 */
export function administersCustomer(me: Me | null, customerId?: string): boolean {
  return (
    me?.roles.some(
      (grant) =>
        grant.role === "administrator" &&
        grant.site_id === null &&
        (!grant.customer_id || customerId === undefined || grant.customer_id === customerId),
    ) ?? false
  );
}
