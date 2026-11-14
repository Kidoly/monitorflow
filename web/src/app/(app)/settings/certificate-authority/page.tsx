import type { Metadata } from "next";

import { CertificateAuthorityPage } from "@/components/CertificateAuthorityPage";

export const metadata: Metadata = { title: "Certificate authority · MonitorFlow" };

export default function CertificateAuthorityRoute() {
  return <CertificateAuthorityPage />;
}
