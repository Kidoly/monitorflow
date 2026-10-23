import type { Metadata } from "next";
import { Suspense } from "react";

import { MirrorListPage } from "@/components/MirrorList";

export const metadata: Metadata = { title: "Mirrors · MonitorFlow" };

// Filters and sort are query parameters.
export default function MirrorsPage() {
  return (
    <Suspense fallback={<p role="status">Loading mirrors…</p>}>
      <MirrorListPage />
    </Suspense>
  );
}
