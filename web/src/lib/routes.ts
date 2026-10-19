/** Link to the page of the server `id` (a query parameter, ADR-0020). */
export function serverHref(id: string): string {
  return `/servers/view/?id=${encodeURIComponent(id)}`;
}
