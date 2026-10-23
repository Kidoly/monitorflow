import type { Metadata } from "next";
import { Suspense } from "react";

import { FleetOverview } from "@/components/FleetOverview";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Fleet · MonitorFlow" };

// Filters, sort and scope live in the query string, read on the client.
export default function FleetPage() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading servers" />}>
      <FleetOverview />
    </Suspense>
  );
}
