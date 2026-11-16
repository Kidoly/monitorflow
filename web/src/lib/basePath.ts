/**
 * The path the web interface is served under: empty for the product, which
 * always has its own address, and set only for a static demo placed under
 * another site's path (docs/demo.md). Next adds it to links and to its own
 * files; plain URLs to files in public/ need `asset`.
 */
export const BASE_PATH = process.env.NEXT_PUBLIC_MONITORFLOW_BASE_PATH ?? "";

/** A file of public/, such as `/brand/logo.svg`, under the base path. */
export function asset(path: string): string {
  return `${BASE_PATH}${path}`;
}
