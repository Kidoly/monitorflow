import type { Metadata } from "next";

import { AiSettingsPage } from "@/components/AiSettings";

export const metadata: Metadata = { title: "AI · MonitorFlow" };

export default function Ai() {
  return <AiSettingsPage />;
}
