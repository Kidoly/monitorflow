import type { RefObject } from "react";

/**
 * The secret of a one-time link (`/setup/#<token>`, `/welcome/#<token>`,
 * an acknowledge link), read once into `held`: the fragment is then
 * removed, keeping the secret out of the history and of anything copied
 * from the address bar. Effects may run twice in development, hence the
 * ref. Empty when the link has none.
 */
export function takeFragmentSecret(held: RefObject<string | null>): string {
  if (held.current === null) {
    held.current = window.location.hash.slice(1);
    if (held.current) window.history.replaceState(null, "", window.location.pathname);
  }
  return held.current;
}
