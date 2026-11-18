import type { Metadata } from "next";

import { ReportsPage } from "@/components/Reports";

export const metadata: Metadata = { title: "Reports · MonitorFlow" };

export default function Page() {
  return <ReportsPage />;
}
