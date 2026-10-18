import type { Metadata } from "next";

import { FleetOverview } from "@/components/FleetOverview";

export const metadata: Metadata = { title: "Fleet · MonitorFlow" };

export default function FleetPage() {
  return <FleetOverview />;
}
