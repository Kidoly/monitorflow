/** The host of a website check's URL (ADR-0150), or the URL when it reads as none. */
export function websiteHost(url: string | undefined): string {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}
