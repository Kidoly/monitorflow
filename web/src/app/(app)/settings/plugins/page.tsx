import type { Metadata } from "next";

import { PluginsPage } from "@/components/PluginsPage";

export const metadata: Metadata = { title: "Plugins · MonitorFlow" };

export default function PluginsRoute() {
  return <PluginsPage />;
}
