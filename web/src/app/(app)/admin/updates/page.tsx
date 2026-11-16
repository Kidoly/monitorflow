import type { Metadata } from "next";
import { Suspense } from "react";

import { AutoUpdatePage } from "@/components/AutoUpdate";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Automatic updates · MonitorFlow" };

// The open list of agents or relays is a query parameter.
export default function UpdatesPage() {
  return (
    <Suspense fallback={<LoadingBlock />}>
      <AutoUpdatePage />
    </Suspense>
  );
}
