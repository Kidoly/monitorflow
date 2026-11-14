import type { Metadata } from "next";

import { SettingsPage } from "@/components/SettingsPage";

export const metadata: Metadata = { title: "General settings · MonitorFlow" };

export default function Settings() {
  return <SettingsPage />;
}
