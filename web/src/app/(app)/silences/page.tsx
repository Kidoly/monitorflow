import type { Metadata } from "next";

import { SilencesPage } from "@/components/Silences";

export const metadata: Metadata = { title: "Silences · MonitorFlow" };

export default function Page() {
  return <SilencesPage />;
}
