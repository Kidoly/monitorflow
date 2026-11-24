import { Suspense } from "react";

import { MobileIncidents } from "@/components/mobile/MobileIncidents";

export default function MobileHomePage() {
  return (
    <Suspense>
      <MobileIncidents />
    </Suspense>
  );
}
