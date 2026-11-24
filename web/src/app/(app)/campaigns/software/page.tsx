import type { Metadata } from "next";

import { SoftwareCatalog } from "@/components/SoftwareCatalog";

export const metadata: Metadata = { title: "Software catalogue · MonitorFlow" };

export default function SoftwareCatalogPage() {
  return <SoftwareCatalog />;
}
