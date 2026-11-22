import type { Metadata } from "next";

import { HaloPsaPage } from "@/components/HaloPsa";

export const metadata: Metadata = { title: "HaloPSA · MonitorFlow" };

export default function HaloPsa() {
  return <HaloPsaPage />;
}
