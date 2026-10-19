import type { Metadata } from "next";
import { Suspense } from "react";

import { IncidentsView } from "@/components/IncidentList";

export const metadata: Metadata = { title: "Incidents · MonitorFlow" };

// Filters live in the query string, read on the client (ADR-0020).
export default function IncidentsPage() {
  return (
    <Suspense fallback={<p role="status">Loading incidents…</p>}>
      <IncidentsView />
    </Suspense>
  );
}
