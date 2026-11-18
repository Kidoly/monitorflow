import type { Metadata } from "next";

import { AddDevicePage } from "@/components/AddDevice";

export const metadata: Metadata = { title: "Add a device · MonitorFlow" };

export default function AddDeviceRoute() {
  return <AddDevicePage />;
}
