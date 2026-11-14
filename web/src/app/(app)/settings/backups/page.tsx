import type { Metadata } from "next";

import { BackupsPage } from "@/components/BackupsPage";

export const metadata: Metadata = { title: "Backups · MonitorFlow" };

export default function BackupsRoute() {
  return <BackupsPage />;
}
