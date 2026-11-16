import type { Metadata } from "next";

import { PreferencesPage } from "@/components/settings/PreferencesPage";

export const metadata: Metadata = { title: "Preferences · MonitorFlow" };

export default function PreferencesRoute() {
  return <PreferencesPage />;
}
