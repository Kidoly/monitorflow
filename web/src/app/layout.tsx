import type { Metadata } from "next";
import type { ReactNode } from "react";

import { LocaleProvider } from "@/i18n/LocaleProvider";
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
      { url: "/favicon.ico", sizes: "16x16 32x32 48x48" },
      { url: "/brand/slime-violette-petit.svg", type: "image/svg+xml" },
    ],
    apple: "/brand/apple-touch-icon.png",
  },
};

export default function RootLayout({ children }: { children: ReactNode }) {
  return (
    // The inline script sets data-theme before hydration, and LocaleProvider
    // sets lang after it, hence the warning opt-out.
    <html lang="en" suppressHydrationWarning>
      <head>
        <script dangerouslySetInnerHTML={{ __html: THEME_SCRIPT }} />
      </head>
      <body>
        <LocaleProvider>{children}</LocaleProvider>
      </body>
    </html>
  );
}
