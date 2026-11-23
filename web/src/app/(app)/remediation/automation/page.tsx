import type { Metadata } from "next";

import { AutomationRules } from "@/components/AutomationRules";

export const metadata: Metadata = { title: "Automation · MonitorFlow" };

export default function AutomationPage() {
  return <AutomationRules />;
}
