import type { Metadata } from "next";

import { TokensAdminPage } from "@/components/TokensAdmin";

export const metadata: Metadata = { title: "Add servers · MonitorFlow" };

export default function TokensPage() {
  return <TokensAdminPage />;
}
