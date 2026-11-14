import type { Metadata } from "next";

import { SettingsIndex } from "@/components/settings/SettingsIndex";

export const metadata: Metadata = { title: "Settings · MonitorFlow" };

export default function Settings() {
  return <SettingsIndex />;
}
