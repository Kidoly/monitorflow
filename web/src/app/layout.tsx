import type { Metadata } from "next";
import type { ReactNode } from "react";

import { THEME_SCRIPT } from "@/lib/theme";

// Inter is bundled with the build: the product is self-hosted and never
// calls a third-party CDN.
import "@fontsource/inter/400.css";
import "@fontsource/inter/500.css";
import "@fontsource/inter/600.css";
import "@fontsource/inter/700.css";
import "./tokens.css";
import "./globals.css";

export const metadata: Metadata = {
  title: "MonitorFlow",
  description: "Monitoring and fleet management",
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    // The inline script sets data-theme before hydration, hence the warning opt-out.
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_SCRIPT }} />
      </head>
      <body>{children}</body>
    </html>
  );
}
