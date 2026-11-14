import type { Metadata } from "next";

import { LicensePage } from "@/components/LicensePage";

export const metadata: Metadata = { title: "License · MonitorFlow" };

export default function LicenseRoute() {
  return <LicensePage />;
}
