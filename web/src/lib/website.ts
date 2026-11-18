/** The host of a website check's URL (ADR-0150), or the URL when it reads as none. */
export function websiteHost(url: string | undefined): string {
  if (!url) return "";
  try {
    return new URL(url).host;
  } catch {
    return url;
  }
}

/** A website check in a list: its host, and its path when it is not the home page ("shop.test/health"). */
export function websiteLabel(url: string | undefined): string {
  if (!url) return "";
  try {
    const parsed = new URL(url);
    const path = parsed.pathname === "/" ? "" : parsed.pathname.replace(/\/$/, "");
    return `${parsed.host}${path}`;
  } catch {
    return url;
  }
}
