import type { Metadata } from "next";

import { SetupForm } from "@/components/SetupForm";

export const metadata: Metadata = { title: "Set up · MonitorFlow" };

export default function SetupPage() {
  return <SetupForm />;
}
