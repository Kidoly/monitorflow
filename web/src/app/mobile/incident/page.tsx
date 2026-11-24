import { Suspense } from "react";

import { MobileIncident } from "@/components/mobile/MobileIncident";

// The incident ID is a query parameter, as on the full page (ADR-0020).
export default function MobileIncidentPage() {
  return (
    <Suspense>
      <MobileIncident />
    </Suspense>
  );
}
