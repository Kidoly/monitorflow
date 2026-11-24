import type { Metadata } from "next";
import { Suspense } from "react";

import { InventorySearchPage } from "@/components/InventorySearch";
import { LoadingBlock } from "@/components/ui/PageState";

export const metadata: Metadata = { title: "Inventory · MonitorFlow" };

export default function InventoryRoute() {
  return (
    <Suspense fallback={<LoadingBlock label="Loading the inventory" />}>
      <InventorySearchPage />
    </Suspense>
  );
}
