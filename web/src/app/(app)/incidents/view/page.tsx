import type { Metadata } from "next";
import { Suspense } from "react";

import { IncidentView } from "@/components/IncidentPage";

export const metadata: Metadata = { title: "Incident · MonitorFlow" };

// The incident ID is a query parameter, like the server page's (ADR-0020).
export default function IncidentViewPage() {
  return (
    <Suspense fallback={<p role="status">Loading incident…</p>}>
      <IncidentView />
    </Suspense>
  );
}
