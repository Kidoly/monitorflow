import type { NextConfig } from "next";

// The production build is a static export (ADR-0014): plain files served
// next to the API by a reverse proxy, no Node runtime. `next dev` has no such
// proxy, so in development only it forwards /api to a local server and the
// session cookie stays same-origin.
const isDev = process.env.NODE_ENV === "development";
const apiUrl = process.env.MONITORFLOW_API_URL ?? "http://127.0.0.1:7444";

const config: NextConfig = {
  output: "export",
  // Every page becomes `<route>/index.html`, which any static file server
  // resolves without extra rules.
  trailingSlash: true,
  reactStrictMode: true,
  poweredByHeader: false,
  ...(isDev && {
    // The API has no trailing slashes; without this, /api/x would be
    // redirected to /api/x/ before the rewrite applies.
    skipTrailingSlashRedirect: true,
    async rewrites() {
      return [{ source: "/api/:path*", destination: `${apiUrl}/api/:path*` }];
    },
  }),
};

export default config;
