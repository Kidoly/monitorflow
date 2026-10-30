import type { Metadata } from "next";

import { WelcomeForm } from "@/components/WelcomeForm";

export const metadata: Metadata = { title: "Choose your password · MonitorFlow" };

export default function WelcomePage() {
  return <WelcomeForm />;
}
