import type { Metadata } from "next";

import { ImportPage } from "@/components/imports/ImportPage";

export const metadata: Metadata = { title: "Move from another tool · MonitorFlow" };

export default function ImportRoute() {
  return <ImportPage />;
}
