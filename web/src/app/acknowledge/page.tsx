import type { Metadata } from "next";

import { AcknowledgeForm } from "@/components/AcknowledgeForm";

export const metadata: Metadata = { title: "Acknowledge · MonitorFlow" };

export default function AcknowledgePage() {
  return <AcknowledgeForm />;
}
