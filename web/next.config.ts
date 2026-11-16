import type { NextConfig } from "next";

// The production build is a static export (ADR-0014): plain files served
// next to the API by a reverse proxy, no Node runtime. `next dev` has no such
// proxy, so in development only it forwards /api to a local server and the
// session cookie stays same-origin.
const isDev = process.env.NODE_ENV === "development";
const apiUrl = process.env.MONITORFLOW_API_URL ?? "http://127.0.0.1:7444";
// The static demo can live under a path of another site, such as /demo on
// the website (docs/demo.md). Only a demo build takes one: the product is
// always served at the root of its own address.
const demo = process.env.NEXT_PUBLIC_MONITORFLOW_DEMO === "1";
const basePath = demo ? (process.env.NEXT_PUBLIC_MONITORFLOW_BASE_PATH ?? "").replace(/\/+$/, "") : "";

const config: NextConfig = {
  output: "export",
  // Every page becomes `<route>/index.html`, which any static file server
  // resolves without extra rules.
  trailingSlash: true,
  reactStrictMode: true,
  poweredByHeader: false,
  ...(basePath && { basePath }),
  // Inlined at build time even when unset, so a normal build compiles the
  // static demo's branch away with its made-up data (docs/demo.md).
  env: {
    NEXT_PUBLIC_MONITORFLOW_DEMO: process.env.NEXT_PUBLIC_MONITORFLOW_DEMO ?? "",
    NEXT_PUBLIC_MONITORFLOW_SITE_URL: process.env.NEXT_PUBLIC_MONITORFLOW_SITE_URL ?? "",
    NEXT_PUBLIC_MONITORFLOW_BASE_PATH: basePath,
  },
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
