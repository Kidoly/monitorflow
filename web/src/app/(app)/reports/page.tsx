import type { Metadata } from "next";

import { BusinessServicesSection } from "@/components/BusinessServices";
import { ReportSchedulesSection } from "@/components/ReportSchedules";
import { ReportsPage } from "@/components/Reports";

export const metadata: Metadata = { title: "Reports · MonitorFlow" };

export default function Page() {
  return (
    <ReportsPage>
      <BusinessServicesSection />
      <ReportSchedulesSection />
    </ReportsPage>
  );
}
