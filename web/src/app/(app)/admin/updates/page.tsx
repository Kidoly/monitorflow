import type { Metadata } from "next";

import { AutoUpdatePage } from "@/components/AutoUpdate";

export const metadata: Metadata = { title: "Automatic updates · MonitorFlow" };

export default function UpdatesPage() {
  return <AutoUpdatePage />;
}
