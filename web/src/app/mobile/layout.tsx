import type { Metadata } from "next";
import type { ReactNode } from "react";

import { MobileShell } from "@/components/mobile/MobileShell";

export const metadata: Metadata = { title: "MonitorFlow" };

// The installed app's pages (ADR-0186), outside the desktop shell.
export default function MobileLayout({ children }: { children: ReactNode }) {
  return <MobileShell>{children}</MobileShell>;
}
