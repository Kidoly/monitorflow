import type { Metadata } from "next";

import { SnmpLibraryPage } from "@/components/SnmpLibrary";

export const metadata: Metadata = { title: "SNMP lookups and MIBs · MonitorFlow" };

export default function SnmpLibrary() {
  return <SnmpLibraryPage />;
}
