import type { Metadata, Viewport } from "next";
import type { ReactNode } from "react";

import { LocaleProvider } from "@/i18n/LocaleProvider";
import { asset } from "@/lib/basePath";
import { THEME_SCRIPT } from "@/lib/theme";

// Inter is bundled with the build: the product is self-hosted and never
// calls a third-party CDN.
import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "./tokens.css";
import "./globals.css";

export const metadata: Metadata = {
  title: "MonitorFlow",
  description: "Monitoring and fleet management",
  icons: {
    icon: [
      { url: asset("/favicon.ico"), sizes: "16x16 32x32 48x48" },
      { url: asset("/brand/slime-violette-petit.svg"), type: "image/svg+xml" },
    ],
    apple: asset("/brand/apple-touch-icon.png"),
  },
  // Installable as an app on a phone (ADR-0186).
  manifest: asset("/manifest.webmanifest"),
  appleWebApp: { capable: true, title: "MonitorFlow", statusBarStyle: "default" },
};

export const viewport: Viewport = {
  width: "device-width",
  initialScale: 1,
  viewportFit: "cover",
  themeColor: "#6a2bd6",
};

/** Set by `npm run build:demo` (docs/demo.md). */
const DEMO = process.env.NEXT_PUBLIC_MONITORFLOW_DEMO === "1";

export default async function RootLayout({ children }: { children: ReactNode }) {
  // Imported only by the demo build, so a normal build leaves it out.
  const DemoBanner = DEMO ? (await import("@/components/DemoBanner")).DemoBanner : null;
  return (
    // The inline script sets data-theme before hydration, and LocaleProvider
    // sets lang after it, hence the warning opt-out.
    <html lang="en" suppressHydrationWarning data-demo={DEMO ? "" : undefined}>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_SCRIPT }} />
      </head>
      <body>
        <LocaleProvider>
          {DemoBanner && <DemoBanner />}
          {children}
        </LocaleProvider>
      </body>
    </html>
  );
}
